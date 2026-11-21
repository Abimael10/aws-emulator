Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.associate-dissociate-packages-concurrent-race-conflict opensearch.e2e-package-association-search-flow opensearch.package-dissociation-blocks-dependent-index-creation-rejected opensearch.update-index-dependency-failure-on-unavailable-package opensearch.update-vpc-endpoint-out-of-range-subnet-rejected opensearch.visibility-timeout-ttl-service-software-rollback-window-accepted
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.associate-dissociate-packages-concurrent-race-conflict|_wait_association_gone_or_status#26|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.e2e-package-association-search-flow|_wait_domain_package_status#22|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.package-dissociation-blocks-dependent-index-creation-rejected|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.update-index-dependency-failure-on-unavailable-package|_wait_domain_package_dissociation_failed#16|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.update-vpc-endpoint-out-of-range-subnet-rejected|_wait_endpoint_active#24|Timeout|time.sleep(interval_s)
Issue-Key: opensearch.visibility-timeout-ttl-service-software-rollback-window-accepted|_wait_service_software_settled#6|Timeout|time.sleep(interval_s)
Issue-Commit: 24e57535a91d23521e4ecdc2ee667ccdedafe408

# opensearch: chained waits for a domain and its dependent resources never converge inside a check's budget — DescribeDomain reports Processing=true on the read taken right after CreateDomain and only settles on a later read, so six package/VPC-endpoint/service-software scenarios never reach a verdict

> _Note: this issue was identified against the code base at commit `24e57535a91d23521e4ecdc2ee667ccdedafe408`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDomain

## Description
Cause 1 of 1: every surviving failure in this suite run is a check that creates a domain and then polls for readiness — of the domain, of a package association, of a service-managed VPC endpoint, or of ServiceSoftwareOptions — and is killed by the suite's 60 s per-check budget before any of its assertions are reached. Failing checks: opensearch.associate-dissociate-packages-concurrent-race-conflict, opensearch.e2e-package-association-search-flow, opensearch.package-dissociation-blocks-dependent-index-creation-rejected, opensearch.update-index-dependency-failure-on-unavailable-package, opensearch.update-vpc-endpoint-out-of-range-subnet-rejected, opensearch.visibility-timeout-ttl-service-software-rollback-window-accepted. All six report only "Error: Check timed out after 60.0s" — no response assertion failed, because none of them ever got far enough to make one.

AWS: Smithy: DescribeDomain -> DomainStatus.Processing — "The status of the domain configuration. True if OpenSearch Service is processing configuration changes. False if the configuration is active." DomainStatus.Created/Processing (plus DomainProcessingStatus Creating/Modifying/Active), DomainPackageDetails.DomainPackageStatus (ASSOCIATING/ACTIVE/DISSOCIATING), VpcEndpoint.Status (CREATING/UPDATING/ACTIVE) and ServiceSoftwareOptions are the readiness signals clients poll; the model states nothing about how long a transient window lasts (recorded behavior only — the recording shows each resource eventually publishing its settled value, and a client polling on a tens-of-seconds interval observing it). AWS's contract here is only that a poll on such a resource converges; the real service does the work in the background and answers with whatever state the resource is actually in at the instant of the read.

Emulator: the emulator models every transient state as an "owed" status with a due instant that is materialised ONLY by a later call that reads the record (services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state.rs: `settle(now)` on each record; the windows are PROVISIONING = 500 ms, DOMAIN_WORK = 3 s, PACKAGE_ASSOCIATION_CHANGE = 750 ms, VPC_ENDPOINT_CHANGE = 3 s). Because the window is deliberately sized to outlast a client's immediate next call, the FIRST DescribeDomain after CreateDomain always answers Created=true/Processing=true (DomainProcessingStatus 'Creating'), so a conventional poll loop sleeps a whole interval (these checks use 10–30 s) before its second read, even though the emulator's own work finished in 3 s. A check that chains two or more such waits therefore spends its entire 60 s deadline sleeping: e.g. visibility-timeout-ttl-service-software-rollback-window-accepted pays one 20 s interval for the domain wait and then needs three consecutive identical ServiceSoftwareOptions reads 20 s apart; update-vpc-endpoint-out-of-range-subnet-rejected waits for the domain, then for CreateVpcEndpoint to leave CREATING; the three package checks wait for the domain and then for DomainPackageStatus to leave ASSOCIATING/DISSOCIATING. In every case the check is terminated with no verdict.

Why: a client that follows the documented sequencing — issue a control-plane call, poll DescribeDomain (or DescribeDomainPackages / DescribeVpcEndpoints) until the resource is no longer in flight, then issue the next call — gets no usable progress from the read it takes immediately after the call: the answer is always "still processing", and the resource only becomes settled because the client polled again. Every operation downstream of such a wait (AssociatePackages/DissociatePackages conflict handling and the ACTIVE settle, CreateIndex against a dissociated analyzer package, DissociatePackages refusal while a live index references the package, UpdateVpcEndpoint cross-VPC subnet rejection, RollbackServiceSoftwareUpdate's 200/RollbackAvailable=false no-op) is unreachable in an environment with a per-call deadline, so clients and tooling see those operations as hung rather than as served — and their refusals and shapes stay unverified.

Note: open issue 20261030T083920Z-opensearch-a-createdomain-updatedomainconfig-sequence-never-reaches-an.md describes the same settle-on-a-later-read behaviour for one other check (opensearch.disabled-operation-gating-across-readonly-apis); it does not carry the replay keys of the six checks listed here, so they are filed as this work item. Fixing the settle path should be verified by re-replaying all of them.

Fix: the domain/package-association/VPC-endpoint/service-software settle path and the owed-window constants in the service's state module, together with the DescribeDomain, DescribeDomainPackages/ListPackagesForDomain, DescribeVpcEndpoints and DescribeDomain(ServiceSoftwareOptions) projections that report status from them: a read taken after the owed work is complete must publish the settled state on THAT read, and the chosen windows must be short enough relative to a client's poll interval that a single readiness wait does not cost a full interval. File citations under Traces.

## Evidence
Expected: each of the six checks completes its sequence and reaches its assertions — a redundant AssociatePackages and a concurrent DissociatePackages landing in the ASSOCIATING window both refused with ConflictException and the association settling to one ACTIVE entry; an associated TXT-DICTIONARY package listed for the domain and its stopwords driving search; CreateIndex refused after the analyzer package is dissociated and accepted after re-association; DissociatePackages ending DISSOCIATION_FAILED while a live index's analyzer references the package; UpdateVpcEndpoint with a subnet from another VPC refused with ValidationException leaving the endpoint unchanged; RollbackServiceSoftwareUpdate returning 200 with RollbackServiceSoftwareOptions.RollbackAvailable=false and CurrentVersion unchanged.

Actual: all six are reported by the suite as [mismatch] with the single detail "Error: Check timed out after 60.0s". No status code, error code or payload was ever asserted — the runs are killed inside poll loops of the form `describe_domain(...)["DomainStatus"]`; `if status.get("Created") and not status.get("Processing"): return`; `time.sleep(interval_s)` with interval_s = 20 or 30 s (and 10 s for the DomainPackageStatus loops). The emulator answers the first such read with Created=true, Processing=true, DomainProcessingStatus='Creating', ChangeProgressDetails.ConfigChangeStatus='ApplyingChanges', so the first iteration always sleeps. Backing state: state.rs declares PROVISIONING = Duration::from_millis(500), DOMAIN_WORK = Duration::from_secs(3), PACKAGE_ASSOCIATION_CHANGE = Duration::from_millis(750), VPC_ENDPOINT_CHANGE = Duration::from_secs(3), with each record's `settle(now)` materialising the owed status only when a later call reads it. Domain names, ARNs, endpoint hosts, package ids and timestamps in these payloads are per-run values and are not part of the requirement.

## Steps to Reproduce
1. CreateDomain(DomainName=<valid name>, EngineVersion=OpenSearch_2.11, ClusterConfig={InstanceType: t3.small.search, InstanceCount: 1, DedicatedMasterEnabled: false, ZoneAwarenessEnabled: false}, EBSOptions={EBSEnabled: true, VolumeType: gp3, VolumeSize: 10}).
2. Immediately call DescribeDomain on that name: DomainStatus.Created=true but Processing=true (DomainProcessingStatus 'Creating'). A poll loop that sleeps its interval on that answer therefore burns a full 20–30 s before its next read, even though the emulator's owed work is due 3 s after the create.
3. Chain a second readiness wait on the same domain and the budget is gone: (a) AssociatePackages(DomainName=<name>, PackageList=[{PackageID: <a package in AVAILABLE>}]) then poll ListPackagesForDomain/DescribeDomainPackages until DomainPackageStatus=ACTIVE at a 10 s interval; or (b) CreateVpcEndpoint(DomainArn=<arn>, VpcOptions={SubnetIds:[<subnet in the domain's VPC>], SecurityGroupIds:[<sg>]}) then poll until Status=ACTIVE; or (c) poll DescribeDomain until ServiceSoftwareOptions is identical on three consecutive reads 20 s apart.
4. With a 60 s per-call/per-check deadline the sequence never reaches the operation under test — DissociatePackages/AssociatePackages conflict handling, CreateIndex against analyzers/<PackageID>, UpdateVpcEndpoint with a subnet from a different VPC, or RollbackServiceSoftwareUpdate — and the run ends with no verdict.
5. Re-replay: opensearch.associate-dissociate-packages-concurrent-race-conflict, opensearch.e2e-package-association-search-flow, opensearch.package-dissociation-blocks-dependent-index-creation-rejected, opensearch.update-index-dependency-failure-on-unavailable-package, opensearch.update-vpc-endpoint-out-of-range-subnet-rejected, opensearch.visibility-timeout-ttl-service-software-rollback-window-accepted.

## Resolution

Cause 1 of 1: DECLINED as to the one thing that is left — the LENGTH of the transient window
each of these records rests in, a per-run timing value the model states nothing about and
AWS's own is minutes of. The half of this report that is a requirement — "a read taken after
the owed work is complete must publish the settled state on THAT read" — is what the emulator
already does, on every one of the surfaces the report names, and it is now pinned by a test
that drives the chains these six checks take.

* What the emulator does today, on every surface this report lists: the read itself
  materialises what the clock owed and then projects it, so the FIRST read taken past the
  span publishes the settled state and no following poll is needed —
  `DescribeDomain`/`DescribeDomainConfig` through `store.settled_mut(…)` → `Domain::settle`
  (`src/state/domains.rs`), which also lands the service software (`ServiceSoftwareOptions`,
  `src/state/service_software.rs`) and the domain's package associations
  (`Domain::settle_package_associations`, reached by `ListPackagesForDomain`,
  `DescribeDomainPackages` and `ListDomainsForPackage` alike), `DescribePackages` through the
  package's own settle (`src/state/packages.rs`), and `DescribeVpcEndpoints` /
  `ListVpcEndpoints` / `ListVpcEndpointsForDomain` through `store.settle_vpc_endpoints(now)`
  (`src/state/vpc_endpoints.rs`).
* Every operation the report lists as unreachable BEHIND those waits is served, and reached
  through the wait rather than around it (new test below): the association settling at
  exactly one `ACTIVE` entry; `CreateIndex` naming a file an associated `TXT-DICTIONARY`
  package provides served while the package is on the domain and refused with the modelled
  `ValidationException`, minting nothing, once it is off, then served again after a
  re-association; `DissociatePackages` left in `DISSOCIATION_FAILED` with the service's
  in-use error details while a live index's analyzer reads the package, and removing the
  association once that index is gone; `UpdateVpcEndpoint` served on the endpoint the wait
  found `ACTIVE` and owed its own span, with the groups it named in place once that span
  passed; and `RollbackServiceSoftwareUpdate`
  answering 200 with `RollbackServiceSoftwareOptions$RollbackAvailable` false and the
  domain's `CurrentVersion` untouched. The parts not restated there are pinned on their
  own axes by suites that already exist, so no second copy of them was added: the redundant
  `AssociatePackages` and the concurrent `DissociatePackages` inside the `ASSOCIATING`
  window both refused HTTP 409 `ConflictException`, by
  `tests/all/package_associations.rs`, `tests/all/package_dissociation.rs` and
  `tests/all/package_dependency_lifecycle.rs` (the last over a table of delays taken from
  inside that span); the cross-VPC subnet refusal of `UpdateVpcEndpoint`
  by `core/router/tests/all/opensearch_vpc_endpoint_vpc_membership.rs` (it needs EC2 mounted
  beside this service, so it lives with the router); the agreement of repeated
  `ServiceSoftwareOptions` reads, by `tests/all/clock_owed_readiness.rs`; and an associated
  dictionary's content driving both the analyze API and a `match` query by
  `tests/all/domain_analysis.rs::an_associated_packages_content_drives_the_analyzer_and_the_search`.
* Why the remaining delta is not a defect: it is the arithmetic of the CHECKS' own poll
  intervals against the replay harness's 60 s per-check budget. A client that reads
  immediately after a call, then sleeps `interval_s`, pays ONE interval per wait for ANY
  non-zero window — the read it takes ~50 ms after the call lands inside a 3 s window and
  inside a 50 ms one alike. These checks chain two to four such waits at 10–30 s, and one of
  them additionally needs three agreeing `ServiceSoftwareOptions` reads 20 s apart (40 s on
  its own), so the budget is spent in the client's `time.sleep` whatever the emulator's
  windows are; that is exactly where all six are cut off. Removing that cost would need a
  window shorter than the latency between a call and the client's next read, i.e. effectively
  zero, which makes the modelled transient state unobservable — `DomainStatus$Processing`
  ("True if OpenSearch Service is processing configuration changes. False if the
  configuration is active") with `DomainProcessingStatus` `Creating`,
  `DomainPackageStatus$ASSOCIATING`/`DISSOCIATING` and `VpcEndpointStatus$CREATING`/`UPDATING`
  are the values a client is told to wait ON — and contradicts this crate's own pinned
  contract: `tests/all/clock_owed_readiness.rs` and `tests/all/successive_domain_transitions.rs`
  (a read taken straight after the call reports the transient status) and
  `tests/all/domain_activity_gates.rs` and `tests/all/package_dependency_lifecycle.rs` (that
  in-flight state is what serializes a second writer, which is what two of these very checks
  grade). AWS's own windows are minutes — far longer than this emulator's — so the same 60 s
  budget would not complete against the real service either, and the recordings gave these
  waits caps of 300–1800 s.
* Replay: all six units still fail, and each on its own key, which is the client's own
  `time.sleep(interval_s)` inside the wait helper — `_wait_association_gone_or_status`,
  `_wait_domain_package_status`, `_poll_until`, `_wait_domain_package_dissociation_failed`,
  `_wait_endpoint_active`, `_wait_service_software_settled` — before the graded call is
  issued. No other cause diverges on them.
* Pinned by `tests/all/clock_owed_readiness/chained_client_waits.rs`: the convergence rule
  over the kinds a chain is built out of (the domain, a package, the association, a VPC
  endpoint and a change to one, the dissociation that FAILS and the one that REMOVES the
  association, and the quiescent service-software read), each look judged by an implication
  rather than a stopwatch — a look taken past the span the service itself records MUST
  already report the terminal state, a look inside it reports the transient one — so the test
  states that a client's wait costs one look past the span and holds on a fast host and a
  loaded one alike. Each wait is then followed by the call it gates, so those calls are
  asserted as reached through the wait. The looking itself is the crate's own
  `domain_lifecycle::poll_until`, and what each gated answer IS stays the sibling suites'
  case, so this file adds one axis rather than a second copy of theirs.
