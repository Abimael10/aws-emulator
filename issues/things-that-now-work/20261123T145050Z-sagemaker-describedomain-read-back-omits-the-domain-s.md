Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-studio-domain-app-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-studio-domain-app-lifecycle
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|DescribeDomain|2xx->2xx|$.AppNetworkAccess
Issue-Commit: a7ab2a4527a82fa9910f5ebaa0f490e37d2a6ca8

# sagemaker DescribeDomain read-back omits the domain's app network-access field

> _Note: this issue was identified against the code base at commit `a7ab2a4527a82fa9910f5ebaa0f490e37d2a6ca8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDomain

## Description
One cause.

Cause 1 of 1: DescribeDomain's response projection serves no `AppNetworkAccess` member for a Studio domain that was created and settled through the emulator's own domain store, so the describe read-back does not report how the domain's non-EFS traffic is routed.

AWS: recorded behavior only for the member name. The on-disk Smithy model (services/service-com-amazonaws-sagemaker-sagemaker/smithy-model/com-amazonaws-sagemaker-sagemaker.json) declares DescribeDomainResponse member `AppNetworkAccessType` targeting enum `AppNetworkAccessType` (values PublicInternetOnly | VpcOnly), documented as "Specifies the VPC used for non-EFS traffic. The default value is PublicInternetOnly"; it declares no member named `AppNetworkAccess`. The recorded AWS response for this step nevertheless carries `AppNetworkAccess` at the top level of the DescribeDomain output, holding the domain's network-access mode (the recording's own value is 'DirectInternetOnly' — that literal is the recording filling the template, not the requirement). The requirement is that DescribeDomain report the domain's app network-access setting under the member name AWS serves, derived from what CreateDomain was given for that domain (or the service default when the create request omitted it), and keep reporting it for the life of the domain. HTTP status is 200 on both sides, so this is a body-shape divergence only, not a status or error-code one.

Emulator: the member is absent from the response — replay reports `$.AppNetworkAccess: missing (expected 'DirectInternetOnly', got None)` with expected_status 200 and actual_status 200. The handler builds its output with `app_network_access_type: Some(domain.app_network_access_type.clone())` and nothing else about network access, so the only network-access key the emulator can ever emit is `AppNetworkAccessType`; there is no code path that emits `AppNetworkAccess`, for any domain or any create input.

Why: a client that reads the domain's network-access mode off DescribeDomain (to decide whether apps in the domain reach the internet directly or only through the customer VPC, to render domain configuration, or to assert a deployment matched its template) reads an absent key. Untyped consumers get a KeyError / nil where AWS always supplies a value, and a client that round-trips a describe back into an update silently drops the setting, because the field it must carry forward was never in the payload it read.

Fix: the DescribeDomain handler's response projection in services/service-com-amazonaws-sagemaker-sagemaker/src/ops/describe_domain.rs — it must populate the recorded `AppNetworkAccess` member from the stored domain's network-access setting alongside the `AppNetworkAccessType` it already reports; if the stored domain carries only the enum form, the domain state written by CreateDomain is the second component to extend so the read path has a value to report rather than defaulting to absence. No other describe-path member diverged on this step, so the status, error channel and the rest of the domain projection are unaffected.

Traces: sagemaker-studio-domain-app-lifecycle#task#7 (the only member in this cluster; affected_count 1). No open issue owns this step — replay reports an empty `owned_by` for it. Nearby open sagemaker issues that do NOT state this requirement: 20261104T071614Z-sagemaker-4-net-new-causes-hyperpod-node-placement.md (CreateDomain 501 / CFN DomainId, behavioral corpus) and 20261123T054636Z-sagemaker-2-net-new-causes-createpresigneddomainurl-s-not.md (CreatePresignedDomainUrl refusal wording).

## Evidence
Replay of fixture `sagemaker-studio-domain-app-lifecycle`, step `task#7`, service `sagemaker`, operation `DescribeDomain`, verdict `mismatch`:

  expected_status: 200, actual_status: 200
  detail: $.AppNetworkAccess: missing (expected 'DirectInternetOnly', got None)
  divergence_key: service=sagemaker|DescribeDomain|status=200->200|keys=$.AppNetworkAccess
  affected: ["sagemaker-studio-domain-app-lifecycle#task#7"] (affected_count 1)
  stub_path: "" (operation is implemented), owned_by: [] (no open issue carries this replay key)

Expected: the DescribeDomain 200 body contains the recorded top-level member `AppNetworkAccess` reporting the domain's network-access mode (the recording shows 'DirectInternetOnly' for this domain; the value follows from the domain's create input / service default, the member's presence is the invariant).
Actual: the body has no `AppNetworkAccess` key at all.

Source of the omission — services/service-com-amazonaws-sagemaker-sagemaker/src/ops/describe_domain.rs:206-244 builds `DescribeDomainOutput` and the only network-access member it sets is line 234, `app_network_access_type: Some(domain.app_network_access_type.clone())`. Model check for the member name: smithy_query(query="operation", operation="DescribeDomain") lists `AppNetworkAccessType` (enum AppNetworkAccessType: PublicInternetOnly, VpcOnly) and no `AppNetworkAccess`, which is why this cause is documented as recorded behavior only.

## Steps to Reproduce
1. Boot the emulator from this checkout and point an AWS client at its data listener.
2. Create a Studio domain: CreateDomain(DomainName=<name>, AuthMode='IAM', DefaultUserSettings={'ExecutionRole': <sagemaker role arn>}, VpcId=<vpc>, SubnetIds=[<subnet>]), and wait for the domain to settle (DescribeDomain Status reaches InService).
3. Call DescribeDomain(DomainId=<the d-… id returned above) and dump the raw 200 response body.
4. Observe the body carries `AppNetworkAccessType` but no `AppNetworkAccess` member, so a client reading the domain's app network-access mode under the name AWS serves finds nothing.
5. Equivalent replay: replay_wire_corpus() and look at fixture `sagemaker-studio-domain-app-lifecycle`, step `task#7` — it reports `$.AppNetworkAccess: missing (expected 'DirectInternetOnly', got None)` at status 200/200.
