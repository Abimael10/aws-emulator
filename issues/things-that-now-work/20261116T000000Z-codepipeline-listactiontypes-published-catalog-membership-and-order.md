Issue-Source: fixer-residue
Issue-Trace: codepipeline-catalog-list-action-types
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codepipeline-catalog-list-action-types
Issue-Shape-Id: com.amazonaws.codepipeline#CodePipeline_20150709
Issue-Key: codepipeline|ListActionTypes|2xx->2xx|$.actionTypes[*].settings.entityUrlTemplate
Issue-Key: codepipeline|ListActionTypes|2xx->2xx|$.actionTypes[*].settings.executionUrlTemplate
Issue-Key: codepipeline|ListActionTypes|2xx->2xx|$.actionTypes[*].settings.revisionUrlTemplate
Issue-Key: codepipeline|ListActionTypes|2xx->2xx|$.actionTypes[*].settings.thirdPartyConfigurationUrl
Issue-Related: 20261111T061123Z-codepipeline-listactiontypes-serves-an-invented-catalog-for-both.md
Issue-Commit: 2c13682b85ef24973b0450dfd30edf77692de3d3

# CodePipeline ListActionTypes: the published catalog's MEMBERSHIP and ORDER still differ from the recorded listing

> _Note: this report is the residue of
> `things-that-now-work/20261111T061123Z-codepipeline-listactiontypes-serves-an-invented-catalog-for-both.md`,
> whose per-entry metadata half is fixed. It is filed separately so the part that is NOT
> fixed stays tracked rather than being closed with it._

## Operation
ListActionTypes

## Description
`ListActionTypes` answers HTTP 200 with the catalog the service publishes itself. Each entry
now carries its provider's detail — every configuration property described and reported as a
key, the provider's artifact bounds, and the provider's URL templates with the Region the
catalog is read for substituted into the console links (`services/service-com-amazonaws-codepipeline-codepipeline-20150709/src/state/built_in_catalog.rs`).
Two things about the LIST itself still differ from the recording, and both are per-position,
so the replay keys above (the four `ActionTypeSettings` members, compared index by index)
keep diverging.

1. MEMBERSHIP. The recording returns 28 `AWS`-owned entries and 5 `ThirdParty`-owned ones.
   The emulator's catalog table carries 15 `AWS`-owned entries and 2 `ThirdParty`-owned ones,
   and publishes 14 further `AWS`-owned identities that `ListActionTypes` does NOT report
   (`AWS_ACTION_TYPES_WITHOUT_DETAIL` in `src/state/action_type_resolution.rs`), which a
   pipeline declaration may name and the declaration validator resolves. So the listing is
   not the same set as the set the service publishes, and a client discovering providers here
   is not shown providers it may legally declare (the CloudFormation deploy action, the
   StackSet actions, `ServiceCatalog`, `AppConfig`, `CodeDeployToECS`, `EKS`,
   `ECRBuildAndPublish`, `InspectorScan`, `Commands`), nor the partner integrations beyond
   `GitHub` and `GhostInspector`.
   What blocks closing it: two of those providers cannot be reported as an `ActionType` at
   all without exceeding the model — the CloudFormation deploy action documents ten input
   artifacts against `MaximumArtifactCount`'s `@range(max: 5)`, and `CodeDeployToECS` and the
   StackSet actions document more configuration parameters than
   `ActionConfigurationPropertyList`'s `@length(max: 10)` can carry
   (`smithy_query query="shape" shape="ActionType,ArtifactDetails"`). The rest need each
   provider's documented parameter table, with the description of every parameter, which has
   to be taken from that provider's own action-structure reference rather than guessed: an
   entry with invented parameters is the defect the related report was filed about.

2. ORDER. The recorded listing leads with `Source/AWS/S3` and then `Invoke/AWS/Lambda`, and
   the `ThirdParty` listing with `Test/ThirdParty/GhostInspector`. The emulator reports one
   total order over the four `ActionTypeId` members (`category`, `owner`, `provider`,
   `version`), which is also what the opaque `nextToken` cursor resumes from, so it leads with
   `Approval/AWS/Manual` and `Source/ThirdParty/GitHub`.
   What blocks closing it: the model declares the operation `@paginated(inputToken:
   "nextToken", outputToken: "nextToken", items: "actionTypes")` with no sort member, and
   neither the model nor the provider documentation states an order for the catalog. The
   recorded sequence is not alphabetical, not the `ActionCategory` enum order, and not
   derivable from any `ActionTypeId` member, so reproducing it would mean hard-coding one
   recording's permutation. If a rule behind it can be established (or if the sequence is an
   artifact of one account's storage and the comparison should be order-insensitive), this is
   the report to settle it in.

## Evidence
`replay_issue_units` after the per-entry-detail fix: verdict `failed`, `other_causes: 0`, all
four keys above `still_failing`, the divergence reported at `$.actionTypes[N]` and
`$.actionTypes[N].settings.*` for essentially every index — the 28-entry recorded list and
the 15-entry published list cannot line up position by position.

Fixed and NOT part of this report (see the related file): the absent
`actionConfigurationProperties[].description`, `key: false` on published properties, the
Region-less console templates, and the partner test type's missing `entityUrlTemplate` /
`executionUrlTemplate` and wrong artifact bounds.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. `ListActionTypes` with `actionOwnerFilter=AWS`: 15 entries, led by `Approval/AWS/Manual`;
   the recording has 28, led by `Source/AWS/S3`.
3. `ListActionTypes` with `actionOwnerFilter=ThirdParty`: 2 entries, led by
   `Source/ThirdParty/GitHub`; the recording has 5, led by
   `Test/ThirdParty/GhostInspector`.
4. Compare the ids the listing reports with the identities the declaration validator
   resolves (`state::action_type_is_published`): the 14 `(category, provider)` pairs in
   `AWS_ACTION_TYPES_WITHOUT_DETAIL` — `Deploy/CloudFormation` among them — are published
   for a declaration but reported by no `ListActionTypes` page.
