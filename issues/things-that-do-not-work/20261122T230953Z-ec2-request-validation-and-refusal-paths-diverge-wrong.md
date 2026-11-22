Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch
Issue-Step: task#15
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch ec2-asgeni-traj-run-instances-ni-device-index-and-placement ec2-asgeni-traj-run-instances-ni-param-conflicts ec2-trajwire-deb-deb-instance-type-casing-and-absent-ami ec2-trajwire-deb-deb-key-pair-named-before-created ec2-trajwire-deb-deb-security-group-name-passed-with-subnet ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override ec2-ibp-traj-launch-template-from-failed-ami-poll ec2-hardware-and-byoip-not-found-error-catalog ec2-ipam-trajwire-provision-cidr-mistakes ec2-ipam-trajwire-read-side-mistakes ec2-ipam-trajwire-reference-path-regional-child-pool ec2-ipam-trajwire-vpc-from-top-pool-then-child ec2-ipam-trajwire-stale-pool-reference ec2-ipam-trajwire-teardown-order-mistakes
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|RunInstances|4xx->4xx|status
Issue-Key: ec2|DescribeLaunchTemplates|4xx->2xx|$.Response
Issue-Key: ec2|DescribeLaunchTemplates|4xx->2xx|status
Issue-Key: ec2|DeleteInstanceConnectEndpoint|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|ProvisionIpamPoolCidr|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|ProvisionIpamPoolCidr|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|ProvisionIpamPoolCidr|2xx->2xx|$.ProvisionIpamPoolCidrResponse.ipamPoolCidr.ipamPoolCidrId
Issue-Key: ec2|CreateVpc|4xx->5xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CreateVpc|4xx->5xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateVpc|4xx->5xx|status
Issue-Key: ec2|DeleteIpam|2xx->4xx|status
Issue-Related: 20261103T071133Z-ec2-invalid-enum-and-attribute-values-are-refused.md
Issue-Related: 20261108T143229Z-ec2-dryrun-is-sequenced-wrongly-against-request-validation.md
Issue-Commit: cbbc12f10a0b2ca1aa413637dc076121229fbbb3

# ec2 request-validation and refusal paths diverge: wrong status, error code, message and minted id across RunInstances, DescribeLaunchTemplates, IPAM and Instance Connect

> _Note: this issue was identified against the code base at commit `cbbc12f10a0b2ca1aa413637dc076121229fbbb3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Eight net-new wire divergences in the ec2 emulator, all of them in the path that decides whether a request is accepted, how it is refused, or what identity the accepted request is given. They share one fault: the per-operation pre-flight sequence (DryRun short-circuit, parameter validation, resource resolution, id minting) does not run in AWS's order and does not use AWS's error classification or wording.

Note on scope: the ec2 Smithy model declares NO modelled errors for any of these operations (ec2Query reports faults through the generic <Response><Errors><Error><Code>/<Message> envelope), so for every error-shaped cause below the AWS side is "recorded behavior only" — the status, Code and Message are taken from the recorded response of the cited fixture step. Where a message has variable parts they are called out explicitly: only the fixed text is a literal requirement, the variable parts must be filled from the request and from emulator state.

There are open ec2 issues that look adjacent but do NOT own any of these replay keys (the replay report lists owned_by: [] for all eight): 20261122T204353Z-ec2-3-net-new-causes-importkeypair-refuses-a.md (ImportKeyPair / CreatePlacementGroup / DescribePrefixLists wording), 20261116T190237Z-ec2-responses-omit-members-aws-always-emits-in.md and 20261101T141251Z-ec2-peering-id-format-prefix-list-and-capacity.md.

Cause 1 of 8: RunInstances runs network-interface device-index validation on a launch-template-supplied interface that AWS accepts, so a DryRun that AWS reports as "would have succeeded" is refused instead.
  AWS: recorded behavior only (RunInstances has no modelled errors; DryRun is RunInstancesRequest.DryRun, xmlName dryRun). With DryRun set and a launch template whose network-interface specification is otherwise valid, AWS answers HTTP 412 with Code "DryRunOperation" and the fixed Message "Request would have succeeded, but DryRun flag is set." (this message has no variable parts). AWS does not treat a single template-supplied interface at deviceIndex 0 as a conflict.
  Emulator: answers HTTP 400, Code "InvalidParameterValue", Message "Value (0) for parameter deviceIndex is invalid. Each network interface must be attached at a device index of its own".
  Why: a client doing a DryRun capacity/permission probe before a real launch reads a hard parameter error, concludes its launch template is malformed, and never launches — even though the identical non-DryRun request succeeds on AWS. The emulator also invents a device-index collision where there is only one interface.
  Fix: the RunInstances handler's network-interface validation (ec2 RunInstances op handler / its request-validation stage) must not raise a device-index collision for a single interface merged in from a launch template, and the DryRun short-circuit must be reached once validation passes.
  Traces: ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch#task#15.

Cause 2 of 8: RunInstances evaluates the DryRun short-circuit BEFORE parameter validation, so an invalid request is reported as a successful dry run.
  AWS: recorded behavior only. A RunInstances request that supplies NetworkInterfaces entries without a DeviceIndex is refused with HTTP 400, Code "InvalidParameterValue" and the fixed Message "Each network interface requires a device index." (no variable parts) — and this refusal is produced even when DryRun is set, because AWS validates the request before deciding the dry-run outcome. The cluster covers the same inversion for several other invalid inputs in the same corpus (unknown AMI / mis-cased instance type, a key pair named before it is created, a security-group NAME passed together with a SubnetId), all of which AWS refuses rather than reporting as a would-have-succeeded dry run.
  Emulator: answers HTTP 412, Code "DryRunOperation", Message "Request would have succeeded, but DryRun flag is set." for all of them.
  Why: DryRun exists precisely so a client can pre-validate. An emulator that answers DryRunOperation for a malformed request makes DryRun useless: the client proceeds to the real call and fails there, and any pre-flight gate built on DryRun passes requests AWS rejects.
  Fix: the RunInstances handler must run the full request-validation stage first and only emit the DryRunOperation refusal when validation has produced no error.
  Traces: ec2-asgeni-traj-run-instances-ni-device-index-and-placement#task#6 (cluster also covers ec2-asgeni-traj-run-instances-ni-device-index-and-placement#task#10, ec2-asgeni-traj-run-instances-ni-param-conflicts#task#7, ec2-trajwire-deb-deb-instance-type-casing-and-absent-ami#task#4, ec2-trajwire-deb-deb-key-pair-named-before-created#task#3, ec2-trajwire-deb-deb-security-group-name-passed-with-subnet#task#4 and #task#5).

Cause 3 of 8: DescribeLaunchTemplates answers 200 for a launch-template NAME that does not exist instead of refusing the request.
  AWS: recorded behavior only (DescribeLaunchTemplates has no modelled errors; LaunchTemplateNames is a request member). When LaunchTemplateNames names a template that does not exist, AWS refuses the whole call with HTTP 400, Code "InvalidLaunchTemplateName.NotFoundException" and the fixed Message "At least one of the launch templates specified in the request does not exist." (no variable parts). Filtering by name is NOT best-effort: an explicit identifier that resolves to nothing is an error, unlike a Filters-based query which legitimately returns an empty set.
  Emulator: answers HTTP 200 with no <Response> error envelope at all (the recorded error body is simply absent), i.e. it silently returns an empty/partial result.
  Why: a caller that deletes or renames a template and then describes it by name is told the template still exists in good standing (empty success), so delete-verification and "does this template exist" probes always report success and teardown/idempotency logic takes the wrong branch.
  Fix: the DescribeLaunchTemplates handler must resolve every explicitly named LaunchTemplateNames/LaunchTemplateIds entry against state and raise the not-found refusal when any one of them is unresolvable, before projecting the result list.
  Traces: ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override#task#9 (cluster also covers ec2-ibp-traj-launch-template-from-failed-ami-poll#teardown#12).

Cause 4 of 8: DeleteInstanceConnectEndpoint's not-found Message is emulator-authored generic-resource wording instead of EC2 Instance Connect's own sentence.
  AWS: recorded behavior only. Deleting an absent EC2 Instance Connect Endpoint answers HTTP 400 with the Message "The Instance Connect Endpoint <endpoint-id> does not exist. Add an existing Instance Connect Endpoint ID and retry your request." — the fixed text is "The Instance Connect Endpoint ", " does not exist. Add an existing Instance Connect Endpoint ID and retry your request."; the only variable part is the endpoint id, taken verbatim from the request's InstanceConnectEndpointId. The status and Code already agree.
  Emulator: emits "The instance-connect-endpoint ID '<endpoint-id>' does not exist" — the generic "The <resource-kind> ID '<id>' does not exist" template this crate uses for every EC2 resource, with the kind rendered as the lowercase-hyphenated shape name and the id quoted.
  Why: clients and operators match on EC2 message text (and some SDK/CLI error-handling and runbooks key off the remediation sentence). The generic template drops the remediation clause entirely and spells the resource differently, so text-based handling that works against AWS does not fire here.
  Fix: the DeleteInstanceConnectEndpoint handler's not-found refusal must use the Instance Connect Endpoint-specific message template rather than the crate's generic resource-not-found formatter.
  Traces: ec2-hardware-and-byoip-not-found-error-catalog#task#16.

Cause 5 of 8: ProvisionIpamPoolCidr refuses NetmaskLength on a top-level pool with the wrong error Code and an emulator-authored Message.
  AWS: recorded behavior only (ProvisionIpamPoolCidr has no modelled errors; NetmaskLength and IpamPoolId are request members, IpamPoolId is required). Providing NetmaskLength for a top-level pool that is not Amazon-PublicIpSource is refused with HTTP 400, Code "InvalidParameterValue" and Message "The request cannot contain Netmask Length for <ipam-pool-id>. A top-level pool without amazon PublicIpSource must use an explicit CIDR." — fixed text "The request cannot contain Netmask Length for " and ". A top-level pool without amazon PublicIpSource must use an explicit CIDR.", with the pool id taken from the request's IpamPoolId.
  Emulator: answers HTTP 400 but with Code "InvalidParameterCombination" and Message "netmaskLength cannot be used to provision BYOIP CIDRs to top-level pools; name the cidr instead" — a different error class and wording that names neither the pool nor the PublicIpSource rule.
  Why: this is a single-parameter constraint, which AWS classes as InvalidParameterValue; a client that branches on InvalidParameterCombination vs InvalidParameterValue (retry-with-CIDR vs re-shape-the-whole-request) takes the wrong branch, and the message does not identify which pool was rejected, so a multi-pool provisioning loop cannot attribute the failure.
  Fix: the ProvisionIpamPoolCidr handler's validation stage must classify the netmask-length-on-top-level-pool constraint as InvalidParameterValue and render AWS's message template with the request's pool id interpolated.
  Traces: ec2-ipam-trajwire-provision-cidr-mistakes#task#6.

Cause 6 of 8: a successful ProvisionIpamPoolCidr returns an ipamPoolCidrId that is not in EC2's IPAM-pool-CIDR id form, so the recorded shape assertion fails.
  AWS: recorded behavior only — the corpus asserts the SHAPE of ProvisionIpamPoolCidrResult.IpamPoolCidr.ipamPoolCidrId (recorded as <shaped-not-asserted>, i.e. the concrete value is per-run but its form is a requirement). EC2 mints IPAM pool CIDR ids as the prefix "ipam-pool-cidr-" followed by a single fixed-width lowercase-hex resource suffix, in the same family as every other EC2 resource id.
  Emulator: emits values of the form "ipam-pool-cidr-" + sixteen literal zero characters + a 32-hex-digit value (observed length far longer than any EC2 resource id, e.g. a 48-character suffix), which does not satisfy the recorded id shape.
  Why: anything that validates or stores EC2 identifiers — SDK/CLI id-form checks, ARN construction, fixed-width columns, log scrapers, and clients that round-trip the id back into DeprovisionIpamPoolCidr — rejects or truncates the emulator's id, so the provisioned CIDR becomes unaddressable even though the call succeeded. (This is a general id-minting defect; the concrete value in the recording is a per-run value and is not itself the requirement.)
  Fix: the IPAM pool-CIDR id minter (ec2 id-generation helper used by the ProvisionIpamPoolCidr handler / state layer) must produce the same prefix+fixed-width-hex form as the crate's other EC2 resource ids instead of zero-padding a full UUID.
  Traces: ec2-ipam-trajwire-provision-cidr-mistakes#task#7 (cluster also covers ec2-ipam-trajwire-provision-cidr-mistakes#task#10 and #task#12, ec2-ipam-trajwire-read-side-mistakes#task#9, ec2-ipam-trajwire-reference-path-regional-child-pool#task#7, ec2-ipam-trajwire-vpc-from-top-pool-then-child#task#12).

Cause 7 of 8: CreateVpc with an IPv6 CIDR requested from an IPAM pool answers 501 NotImplemented, so a reference to a pool that does not exist is never validated.
  AWS: recorded behavior only. CreateVpc naming an IPv6 IPAM pool that does not exist is refused with HTTP 400, Code "InvalidIpamPoolId.NotFound" and Message "The ipam-pool ID '<ipam-pool-id>' does not exist" — fixed text "The ipam-pool ID '" and "' does not exist", with the pool id taken from the request's Ipv6IpamPoolId. AWS resolves the referenced pool before doing anything else.
  Emulator: answers HTTP 501 with Code "NotImplemented" and Message "operation CreateVpc with an IPv6 CIDR block allocated from an IPAM pool is scaffolded but not implemented in this emulator". (CreateVpc itself is implemented — the replay report carries no stub_path for this cluster — it is only this request shape that bails out.)
  Why: 501 is not a client error, so SDK retry policies treat it as a server fault and retry, and no client can distinguish "this emulator cannot do IPAM-backed VPCs" from "the pool I named is gone". Any IPAM-driven VPC provisioning flow stalls instead of getting the actionable not-found.
  Fix: the CreateVpc handler must resolve Ipv6IpamPoolId against IPAM state and emit the InvalidIpamPoolId.NotFound refusal for an unknown pool before reaching the not-implemented guard; the IPv6-from-IPAM allocation path then needs implementing in that handler.
  Traces: ec2-ipam-trajwire-stale-pool-reference#task#6.

Cause 8 of 8: DeleteIpam is refused 400 for an IPAM that AWS deletes successfully, so IPAM teardown cannot complete.
  AWS: recorded behavior only (DeleteIpam takes IpamId (required) and Cascade; it has no modelled errors). At the recorded point in the teardown sequence — after the pool CIDRs and pools under the IPAM have been released — DeleteIpam answers HTTP 200 and returns the deleted Ipam in DeleteIpamResult.ipam.
  Emulator: answers HTTP 400 (the recorded comparison reports only the status divergence), i.e. it still believes the IPAM has live dependents and refuses the delete.
  Why: the IPAM can never be deleted, so every scenario that provisions an IPAM leaks it; teardown fails, and a client that polls delete-until-success loops forever against an error that will never clear.
  Fix: the DeleteIpam handler's dependency check (and the state bookkeeping that is supposed to clear scope/pool/CIDR back-references when those children are deprovisioned) must agree with the order in which the children were actually released, so a fully drained IPAM deletes.
  Traces: ec2-ipam-trajwire-teardown-order-mistakes#teardown#19.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up: true, bootstrap_signal: false, 4230 steps, 3533 passed, 622 mismatched, score 83.5%). All eight clusters below are reported with owned_by: [] and stub_path: "".

Cause 1 — ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch#task#15, RunInstances:
  expected status 412 / actual 400
  $.Response.Errors.Error.Code: expected 'DryRunOperation', got 'InvalidParameterValue'
  $.Response.Errors.Error.Message: expected 'Request would have succeeded, but DryRun flag is set.', got 'Value (0) for parameter deviceIndex is invalid. Each network interface must be attached at a device index of its own'

Cause 2 — ec2-asgeni-traj-run-instances-ni-device-index-and-placement#task#6, RunInstances (7 affected steps):
  expected status 400 / actual 412
  $.Response.Errors.Error.Code: expected 'InvalidParameterValue', got 'DryRunOperation'
  $.Response.Errors.Error.Message: expected 'Each network interface requires a device index.', got 'Request would have succeeded, but DryRun flag is set.'

Cause 3 — ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override#task#9, DescribeLaunchTemplates (2 affected steps):
  expected status 400 / actual 200
  $.Response: missing — expected {'Errors': {'Error': {'Code': 'InvalidLaunchTemplateName.NotFoundException', 'Message': 'At least one of the launch templates specified in the request does not exist.'}}, 'RequestID': '<shaped-not-asserted>'}, got None

Cause 4 — ec2-hardware-and-byoip-not-found-error-catalog#task#16, DeleteInstanceConnectEndpoint (status 400 both sides):
  $.Response.Errors.Error.Message: expected 'The Instance Connect Endpoint eice-00000000000000000 does not exist. Add an existing Instance Connect Endpoint ID and retry your request.', got "The instance-connect-endpoint ID 'eice-00000000000000000' does not exist"
  (the endpoint id shown is the value from that request; the requirement is the template, not that id)

Cause 5 — ec2-ipam-trajwire-provision-cidr-mistakes#task#6, ProvisionIpamPoolCidr (status 400 both sides):
  $.Response.Errors.Error.Code: expected 'InvalidParameterValue', got 'InvalidParameterCombination'
  $.Response.Errors.Error.Message: expected 'The request cannot contain Netmask Length for ipam-pool-000000000000000ac. A top-level pool without amazon PublicIpSource must use an explicit CIDR.', got 'netmaskLength cannot be used to provision BYOIP CIDRs to top-level pools; name the cidr instead'

Cause 6 — ec2-ipam-trajwire-provision-cidr-mistakes#task#7, ProvisionIpamPoolCidr (status 200 both sides, 6 affected steps):
  $.ProvisionIpamPoolCidrResponse.ipamPoolCidr.ipamPoolCidrId: expected '<shaped-not-asserted>' (shape assertion), got 'ipam-pool-cidr-00000000000000ad2412af73fdee7cfc'

Cause 7 — ec2-ipam-trajwire-stale-pool-reference#task#6, CreateVpc:
  expected status 400 / actual 501
  $.Response.Errors.Error.Code: expected 'InvalidIpamPoolId.NotFound', got 'NotImplemented'
  $.Response.Errors.Error.Message: expected "The ipam-pool ID 'ipam-pool-0000000000000000f' does not exist", got 'operation CreateVpc with an IPv6 CIDR block allocated from an IPAM pool is scaffolded but not implemented in this emulator'

Cause 8 — ec2-ipam-trajwire-teardown-order-mistakes#teardown#19, DeleteIpam:
  expected status 200 / actual 400

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this checkout.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1) against it.
3. Inspect the following fixture steps, each of which reports a mismatch cluster with owned_by: [] and stub_path: "":
   - ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch#task#15 (RunInstances, DryRun probe on a launch template whose network interface AWS accepts)
   - ec2-asgeni-traj-run-instances-ni-device-index-and-placement#task#6 (RunInstances, DryRun set on a request whose NetworkInterfaces entry omits DeviceIndex)
   - ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override#task#9 (DescribeLaunchTemplates on a LaunchTemplateNames value that does not exist)
   - ec2-hardware-and-byoip-not-found-error-catalog#task#16 (DeleteInstanceConnectEndpoint on an absent InstanceConnectEndpointId)
   - ec2-ipam-trajwire-provision-cidr-mistakes#task#6 (ProvisionIpamPoolCidr with NetmaskLength against a top-level, non-Amazon-PublicIpSource pool)
   - ec2-ipam-trajwire-provision-cidr-mistakes#task#7 (ProvisionIpamPoolCidr success — check the shape of ipamPoolCidr.ipamPoolCidrId)
   - ec2-ipam-trajwire-stale-pool-reference#task#6 (CreateVpc naming an Ipv6IpamPoolId that does not exist)
   - ec2-ipam-trajwire-teardown-order-mistakes#teardown#19 (DeleteIpam after its pools and pool CIDRs have been released)
4. Compare HTTP status, $.Response.Errors.Error.Code and $.Response.Errors.Error.Message (and, for the ProvisionIpamPoolCidr success, the ipamPoolCidrId form) against the recorded response for each step.
5. All eight must be closed: re-replay every fixture#step listed above, not just the representative.
