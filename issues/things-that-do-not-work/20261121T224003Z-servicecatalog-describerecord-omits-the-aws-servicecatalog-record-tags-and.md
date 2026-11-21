Issue-Source: trace-sweep-divergence
Issue-Trace: servicecatalog-provision-product-returns-template-outputs
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: servicecatalog-provision-product-returns-template-outputs
Issue-Shape-Id: com.amazonaws.servicecatalog#AWS242ServiceCatalogService
Issue-Key: servicecatalog|DescribeRecord|2xx->2xx|$.RecordDetail.RecordTags[*]
Issue-Key: servicecatalog|DescribeRecord|2xx->2xx|$.RecordOutputs[*].Description
Issue-Commit: 1945f04af551ab9122ba85c8ecd7b899fcb2daf4

# DescribeRecord omits the aws:servicecatalog record tags and mis-reports the launch stack-ARN output

> _Note: this issue was identified against the code base at commit `1945f04af551ab9122ba85c8ecd7b899fcb2daf4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeRecord

## Description
DescribeRecord answers HTTP 200 for a successful PROVISION_PRODUCT record, but the RecordDetail/RecordOutputs it reports are missing the metadata AWS attaches to a launch record. Two distinct causes, both observed on the same replayed step.

Cause 1 of 2: RecordDetail.RecordTags is reported empty for a PROVISION_PRODUCT record, so none of the AWS-managed `aws:servicecatalog:*` tags appear.
  AWS: Smithy: DescribeRecord -> DescribeRecordOutput.RecordDetail (RecordDetail), member RecordTags -> RecordTags (list of RecordTag{Key: RecordTagKey len 1..128, Value: RecordTagValue len 1..256}), documented "One or more tags."; the model does not enumerate which tags a record carries, so the tag SET is recorded behavior only. The recording shows that for a record of a successful ProvisionProduct, Service Catalog itself adds five service-managed tags describing what was provisioned, reported in this order: `aws:servicecatalog:productArn` (the ARN of the product provisioned), `aws:servicecatalog:provisioningPrincipalArn` (the ARN of the principal that called ProvisionProduct), `aws:servicecatalog:provisioningArtifactIdentifier` (the provisioning-artifact id used), `aws:servicecatalog:portfolioArn` (the ARN of the portfolio the launch path went through), and `aws:servicecatalog:provisionedProductArn` (the ARN of the provisioned product the launch created). Those five keys are the fixed part of the requirement; each Value is derived from the request and the emulator's own state (the product id, the calling principal, the artifact id, the portfolio id, the provisioned-product id and the account/region the emulator serves) and must never be the literal ARNs of any one recording.
  Emulator: RecordTags is serialized from the record's user-supplied tag map only, so a ProvisionProduct request that passed no Tags yields an empty RecordTags list and every `aws:servicecatalog:*` entry is absent (replay reports RecordTags[0..4] as `missing ... got None`).
  Why: a client that reads a launch record to learn which product, version, portfolio, principal and provisioned product a record belongs to (the documented way to correlate a record back to its inputs without re-describing each resource) finds no tags at all; code that looks up `aws:servicecatalog:productArn` or `aws:servicecatalog:provisionedProductArn` gets a key error or silently treats the launch as untagged.
  Fix: the record's state representation — the RecordDetail/RecordTags projection for a record, which must merge the service-managed tags for a PROVISION_PRODUCT record with the caller's own tags, rather than the handler's serializer.
  Traces: servicecatalog-provision-product-returns-template-outputs#task#10 (DescribeRecord, expected 200, got 200 with RecordTags[0..4] missing). Code: services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice/src/state.rs:1151-1184 (`Record::detail` sets `record_tags: Some(self.tag_list())`, and `tag_list` maps only the stored user tag map).

Cause 2 of 2: the launch's CloudFormation stack output is reported last and with no Description, instead of first with AWS's description.
  AWS: Smithy: DescribeRecord -> DescribeRecordOutput.RecordOutputs (list of RecordOutput{OutputKey, OutputValue, Description}), Description documented "The description of the output." The model does not fix the order or the description text, so this is recorded behavior only: Service Catalog reports the stack a CloudFormation-backed launch stood up as the FIRST record output, with the fixed Description text "The ARN of the launched Cloudformation Stack", ahead of the outputs the product's own template declares. Only the OutputValue of that entry is variable (the stack ARN as CloudFormation addresses it, account/region/stack-id specific); the description is fixed text.
  Emulator: the stack-ARN output is APPENDED after the template's own outputs and is built with `description: None`, so RecordOutputs[0] is the template's first output and its Description is the template's own text (replay: `$.RecordOutputs[0].Description` expected 'The ARN of the launched Cloudformation Stack', got 'The Env parameter the launch passed').
  Why: a client that reads RecordOutputs[0], or that keys off the stack-ARN output's Description to present the launched stack, reads a template output instead of the stack, and the stack entry it does find carries no description at all — so the launched stack cannot be identified from the record the way it can on AWS.
  Fix: the state/seam that assembles a launch's record outputs — place the stack entry before the template's outputs and give it AWS's fixed Description — not the DescribeRecord handler's paging logic (paging then pages the AWS-ordered list).
  Traces: servicecatalog-provision-product-returns-template-outputs#task#10 (same step). Code: services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice/src/state/cloudformation.rs:390-411 (`launch_outputs` collects the template outputs, then pushes `STACK_ARN_OUTPUT` with `description: None`); consumed at services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice/src/ops/describe_record.rs:200-248.

Neither cause is covered by an existing open issue. The open issue `20261115T064756Z-servicecatalog-describerecord-stub.md` asks for DescribeRecord to be implemented at all (it recorded a 501 scaffold); the operation now answers 200, and these are fidelity gaps in that answer. The open bundles `20261029T191224Z-servicecatalog-service-catalog-portfolio-and-product-operations-diverge-from.md` and `20261115T121626Z-servicecatalog-provisioned-product-outputs-and-product-view-lookups.md` cover portfolio/product error messages and GetProvisionedProductOutputs/DescribeProductView lookups, not DescribeRecord's record tags or output ordering.

## Evidence
Replay of fixture `servicecatalog-provision-product-returns-template-outputs`, step `task#10` (DescribeRecord): expected status 200, actual status 200, body diverges.

Cause 1 — RecordDetail.RecordTags:
  expected RecordTags[0] = {Key: 'aws:servicecatalog:productArn', Value: <the provisioned product's product ARN>}
  expected RecordTags[1] = {Key: 'aws:servicecatalog:provisioningPrincipalArn', Value: <shaped-not-asserted>}
  expected RecordTags[2] = {Key: 'aws:servicecatalog:provisioningArtifactIdentifier', Value: <the pa-… id used by the launch>}
  expected RecordTags[3] = {Key: 'aws:servicecatalog:portfolioArn', Value: <the port-… portfolio ARN of the launch path>}
  expected RecordTags[4] = {Key: 'aws:servicecatalog:provisionedProductArn', Value: <the provisioned product's stack ARN>}
  actual: all five missing (None) — RecordTags is an empty list.
  Fixed part of the requirement: the five tag keys and their presence on a successful PROVISION_PRODUCT record. Variable part: every Value (product id/ARN, principal ARN, pa- id, port- ARN, pp- ARN, account and region), taken from the request and emulator state — the ARNs in the recording are one run's values, not the contract.

Cause 2 — RecordOutputs[0].Description:
  expected 'The ARN of the launched Cloudformation Stack'
  actual   'The Env parameter the launch passed'
  The expected string is entirely fixed text belonging to the `CloudformationStackARN` output; the actual string is the description of a template-declared output, which surfaces only because the stack entry is appended last (and with Description absent) instead of being reported first.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture `servicecatalog-provision-product-returns-template-outputs`.
2. The fixture creates a portfolio and a product, associates them, and calls ProvisionProduct with a CloudFormation-backed provisioning artifact that declares template outputs.
3. At step `task#10` call DescribeRecord with the RecordId that ProvisionProduct returned, after the record reaches status SUCCEEDED.
4. Observe HTTP 200 whose RecordDetail.RecordTags is an empty list — none of aws:servicecatalog:productArn, :provisioningPrincipalArn, :provisioningArtifactIdentifier, :portfolioArn, :provisionedProductArn is present.
5. Observe RecordOutputs[0] is a template-declared output rather than the `CloudformationStackARN` entry, and that the `CloudformationStackARN` entry appears last with no Description instead of first with Description 'The ARN of the launched Cloudformation Stack'.
