Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: servicecatalog.provisioned-product-creates-cloudformation-stack servicecatalog.portfolio-tagoption-reaches-cloudformation-stack
Issue-Shape-Id: com.amazonaws.servicecatalog#AWS242ServiceCatalogService
Issue-Key: servicecatalog.provisioned-product-creates-cloudformation-stack|run#29|AssertionError|assert stack_tags.get("aws:servicecatalog:portfolioArn", "").endswith(f"/{portfolio_id}"), (
Issue-Key: servicecatalog.portfolio-tagoption-reaches-cloudformation-stack|run#36|AssertionError|assert stack_tags.get(tag_key) == tag_value, (
Issue-Commit: 52dd5548c9dac981d1505a1c67d2263892a7273a

# ProvisionProduct does not stamp the launch's governance tags (aws:servicecatalog:*) or the portfolio's TagOptions onto the CloudFormation stack it deploys

> _Note: this issue was identified against the code base at commit `52dd5548c9dac981d1505a1c67d2263892a7273a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ProvisionProduct

## Description
Two net-new root causes in Service Catalog provisioning, both about tags the service itself must place on the CloudFormation stack it creates for a provisioned product. The emulator creates the stack and the launch settles AVAILABLE, so the provisioning pipeline works; only the tag set it writes onto the stack is wrong.

Cause 1 of 2: ProvisionProduct omits the aws:servicecatalog:* governance tags on the deployed stack.
AWS: recorded behavior only — the Smithy model for ProvisionProduct models only the caller's own Tags (ProvisionProductInput.Tags, target Tags, @length min 0 max 50) and says nothing about the stack's tag set, so the requirement comes from the recording. In AWS, the stack Service Catalog creates for a provisioned product (named SC-<account>-<provisionedProductId>) carries, in addition to the caller's Tags, five service-applied tags that record the graph the launch went through: `aws:servicecatalog:portfolioArn` = the ARN of the portfolio whose launch path granted the launch (ends in `/<portfolioId>`), `aws:servicecatalog:productArn` = the ARN of the launched product (ends in `/<productId>`), `aws:servicecatalog:provisioningArtifactIdentifier` = the ProvisioningArtifactId of the launch, `aws:servicecatalog:provisioningPrincipalArn` = the ARN of the calling principal/session that invoked ProvisionProduct, and `aws:servicecatalog:provisionedProductArn` = the ARN of the provisioned product (ends in `/<provisionedProductId>`). All five are values derived from the request and from the emulator's own state (the resolved launch path, the artifact id, the caller identity, the generated provisioned-product id) — not literals from any one run.
Emulator: DescribeStacks on the stack ARN that GetProvisionedProductOutputs reports as `CloudformationStackARN` returns Tags containing only the caller-supplied tag; `aws:servicecatalog:portfolioArn` reads None (`the stack's aws:servicecatalog:portfolioArn tag is None, which does not name the granting portfolio 'port-...'`). The other four service-applied tags are absent in the same way.
Why: these tags are the only machine-readable link from a deployed stack back to the portfolio, product, artifact, principal and provisioned product that produced it. Clients that enforce governance, attribute cost, or scope IAM/Config/tag-policy rules on `aws:servicecatalog:*` see an untagged stack and treat a governed deployment as unmanaged; drift/ownership tooling cannot resolve the stack to its provisioned product at all.
Fix: the ProvisionProduct handler (and the state it records for the provisioned product), where it composes the tag list passed to the stack it creates — the five service-applied tags must be added to the caller's Tags from the resolved launch graph and caller identity.
Traces: servicecatalog.provisioned-product-creates-cloudformation-stack.

Cause 2 of 2: a TagOption associated with the portfolio is offered on the launch path but never applied to the deployed stack.
AWS: recorded behavior only for the propagation step; the model supports the first hop (DescribeProvisioningParameters output member `TagOptions`, target TagOptionSummaries). In AWS, a TagOption created with CreateTagOption and associated to a PORTFOLIO with AssociateTagOptionWithResource is inherited by every product launched through that portfolio's path: DescribeProvisioningParameters offers it under TagOptions as {Key: [Value]}, and ProvisionProduct applies it to the deployed stack as a tag Key=<the TagOption key>, Value=<the TagOption value>, without the caller ever passing it in Tags. The key and value come from the TagOption in state, not from the request.
Emulator: after CreateTagOption + AssociateTagOptionWithResource(ResourceId=<portfolioId>) and a successful launch through that portfolio's path, DescribeStacks on the deployed stack returns tags consisting only of the caller-supplied tag: `the portfolio's TagOption 'sim2realtagopt...' did not reach the deployed stack: its tags are ['sim2real'] and 'sim2realtagopt...' reads None, expected 'tva...'`. The portfolio-inherited TagOption is dropped between the launch path and the stack.
Why: TagOptions are Service Catalog's mechanism for administrators to force tags onto everything launched from a portfolio. A client relying on AWS's behavior believes mandatory tagging is enforced centrally, while the emulator deploys untagged stacks — tag-based cost allocation, tag policies and tag-conditioned IAM all silently fail, and the enforcement cannot be detected from the API responses because the launch reports success.
Fix: the ProvisionProduct handler's tag composition, which must merge the TagOptions inherited from the portfolio on the resolved launch path (the same set DescribeProvisioningParameters already reports) into the stack's tags.
Traces: servicecatalog.portfolio-tagoption-reaches-cloudformation-stack.

## Evidence
Check servicecatalog.provisioned-product-creates-cloudformation-stack (mismatch):
  stack_tags = {t['Key']: t['Value'] for t in cfn.describe_stacks(StackName=stack_arn)['Stacks'][0]['Tags']}
  assert stack_tags.get('aws:servicecatalog:portfolioArn','').endswith(f'/{portfolio_id}')
  AssertionError: the stack's aws:servicecatalog:portfolioArn tag is None, which does not name the granting portfolio 'port-s2h7dnp4inzg'
  Expected: Tags include aws:servicecatalog:portfolioArn (ends '/<portfolioId>'), aws:servicecatalog:productArn (ends '/<productId>'), aws:servicecatalog:provisioningArtifactIdentifier == <ProvisioningArtifactId>, aws:servicecatalog:provisioningPrincipalArn == caller session ARN, aws:servicecatalog:provisionedProductArn (ends '/<provisionedProductId>'), plus the caller's Tags.
  Actual: only the caller-supplied tag is present; every aws:servicecatalog:* tag is absent.
  Earlier assertions in the same check passed, so the stack exists, is CREATE_COMPLETE, is named SC-<account>-<pp_id>, carries the ProvisioningParameters and returns its outputs.

Check servicecatalog.portfolio-tagoption-reaches-cloudformation-stack (mismatch):
  assert stack_tags.get(tag_key) == tag_value
  AssertionError: the portfolio's TagOption 'sim2realtagopt7fcfb57b8489' did not reach the deployed stack: its tags are ['sim2real'] and 'sim2realtagopt7fcfb57b8489' reads None, expected 'tva2708e3ac189'
  Expected: the deployed stack's Tags contain <TagOption key> = <TagOption value> for the TagOption associated with the launching portfolio, in addition to the caller's Tags.
  Actual: stack Tags = ['sim2real'] only (the caller's own tag); the inherited TagOption is missing.
  The hop before it passed: DescribeProvisioningParameters did offer TagOptions {<key>: [<value>]} inherited from the portfolio, and ListResourcesForTagOption reported the TagOption on the portfolio — so the association is modelled; only propagation to the stack is missing.

Smithy (service-com-amazonaws-servicecatalog-aws242servicecatalogservice): ProvisionProduct input Tags -> Tags @length(0,50); DescribeProvisioningParameters output TagOptions -> TagOptionSummaries. Neither models the stack's tag set, so the stack-tag requirements above are recorded behavior.

## Steps to Reproduce
Cause 1 (aws:servicecatalog:* governance tags):
1. Upload a minimal CloudFormation template to S3 and CreateProduct with a provisioning artifact pointing at it.
2. CreatePortfolio, AssociateProductWithPortfolio, AssociatePrincipalWithPortfolio for the calling principal.
3. ListLaunchPaths(ProductId=<productId>) and take the path Id.
4. ProvisionProduct(ProductId, ProvisioningArtifactId, PathId, ProvisionedProductName, ProvisionToken, ProvisioningParameters=[{Key:'Env',Value:'prod'}], Tags=[{Key:'sim2real',Value:'...'}]); poll DescribeProvisionedProduct until AVAILABLE.
5. GetProvisionedProductOutputs(ProvisionedProductId=<ppId>) and read the CloudformationStackARN output.
6. cfn.DescribeStacks(StackName=<that ARN>) and inspect Stacks[0].Tags -> the five aws:servicecatalog:* tags are absent; AWS returns them alongside the caller's tag.

Cause 2 (portfolio TagOption):
1. CreateTagOption(Key=<k>, Value=<v>), then AssociateTagOptionWithResource(ResourceId=<portfolioId>, TagOptionId=<id>) on the portfolio from step 2 above (do NOT tag the product and do NOT pass <k> in Tags).
2. DescribeProvisioningParameters(ProductId, ProvisioningArtifactId, PathId) -> TagOptions correctly offers {<k>: [<v>]}.
3. ProvisionProduct through that path with Tags=[{Key:'sim2real',Value:'...'}] only; wait for AVAILABLE.
4. GetProvisionedProductOutputs -> CloudformationStackARN; cfn.DescribeStacks(StackName=<ARN>).Stacks[0].Tags -> contains only 'sim2real'; AWS also contains <k>=<v>.
