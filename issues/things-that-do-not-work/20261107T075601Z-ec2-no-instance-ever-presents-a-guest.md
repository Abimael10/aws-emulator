Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.arm64-t4g-micro-guest-reports-aarch64 ec2.arm64-x86-64-binary-execution-in-the-guest-rejected ec2.arm64-x86-64-payload-runs-on-t3-micro-and-is-rejected-on-t4g-micro ec2.cpuopts-threads-per-core-one-observed-in-guest ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest ec2.capres-open-reservation-consumed-implicitly-while-a-targeted-one-is-untouched ec2.concurrent-modify-instance-capacity-reservation-attributes-race ec2.diops-io2-provisioned-iops-outrun-gp3-baseline-measured-by-fio-in-one-guest ec2.dsnapinc-changed-blocks-across-unrelated-lineages-rejected-same-lineage-lists-guest-block ec2.dvolgrow-capacity-added-by-modify-volume-is-unusable-until-resize2fs ec2.dvolgrow-root-volume-grows-online-and-only-growpart-reaches-the-live-root-filesystem ec2.ebsio-snapshot-restored-into-another-availability-zone-carries-the-payload ec2.ec2-attach-volume-across-availability-zones-rejected ec2.ec2-concurrent-attach-of-one-volume-to-two-instances-yields-a-single-winner ec2.ec2-ebs-volume-bytes-survive-detach-and-reattach-to-a-different-instance ec2.ec2-instance-store-bytes-vanish-across-stop-start-while-ebs-bytes-persist ec2.ec2-instance-type-change-is-visible-to-the-guest-and-matches-the-catalog ec2.ec2-instance-user-data-executes-and-the-guest-reports-the-nonce ec2.ec2-launch-with-a-private-ip-already-in-use-rejected ec2.ec2-security-group-ingress-decides-whether-the-instance-answers ec2.ec2-snapshot-restores-the-exact-bytes-written-to-the-source-volume ec2.ec2-stop-start-preserves-root-volume-bytes-and-reassigns-the-public-ip ec2.hibernate-restores-the-running-process-and-a-plain-stop-does-not ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document ec2.idvdoc-signature-verifies-against-the-aws-public-certificate ec2.imdshop-container-on-the-default-bridge-blocked-at-hop-limit-1-and-served-at-2 ec2.multiattach-detaching-one-guest-leaves-the-other-attachment-live-and-its-bytes-intact ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot ec2.secip-assigning-more-private-ips-than-the-instance-type-permits-rejected ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address ec2.secip-secondary-private-ip-is-reachable-from-a-peer-only-when-the-eni-holds-it ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic ec2.asghc-launch-template-round-trips-imdsv2-user-data-and-its-omissions
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.arm64-t4g-micro-guest-reports-aarch64|_poll#17|Timeout|time.sleep(interval_s)
Issue-Key: ec2.arm64-x86-64-binary-execution-in-the-guest-rejected|_poll#18|Timeout|time.sleep(interval_s)
Issue-Key: ec2.arm64-x86-64-payload-runs-on-t3-micro-and-is-rejected-on-t4g-micro|_poll#18|Timeout|time.sleep(interval_s)
Issue-Key: ec2.cpuopts-threads-per-core-one-observed-in-guest|_poll#21|Timeout|time.sleep(interval_s)
Issue-Key: ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest|_poll#12|Timeout|time.sleep(interval_s)
Issue-Key: ec2.capres-open-reservation-consumed-implicitly-while-a-targeted-one-is-untouched|_poll#18|Timeout|time.sleep(interval_s)
Issue-Key: ec2.concurrent-modify-instance-capacity-reservation-attributes-race|_poll_until#20|Timeout|time.sleep(interval_s)
Issue-Key: ec2.diops-io2-provisioned-iops-outrun-gp3-baseline-measured-by-fio-in-one-guest|_poll#18|Timeout|time.sleep(interval_s)
Issue-Key: ec2.dsnapinc-changed-blocks-across-unrelated-lineages-rejected-same-lineage-lists-guest-block|_run_guest#29|Timeout|time.sleep(5)
Issue-Key: ec2.dvolgrow-capacity-added-by-modify-volume-is-unusable-until-resize2fs|_run_guest#26|Timeout|time.sleep(5)
Issue-Key: ec2.dvolgrow-root-volume-grows-online-and-only-growpart-reaches-the-live-root-filesystem|_run_guest#33|Timeout|time.sleep(5)
Issue-Key: ec2.ebsio-snapshot-restored-into-another-availability-zone-carries-the-payload|_poll_until#20|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-attach-volume-across-availability-zones-rejected|_poll_until#33|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-concurrent-attach-of-one-volume-to-two-instances-yields-a-single-winner|_poll_until#26|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-ebs-volume-bytes-survive-detach-and-reattach-to-a-different-instance|_poll_until#20|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-instance-store-bytes-vanish-across-stop-start-while-ebs-bytes-persist|_poll#27|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-instance-type-change-is-visible-to-the-guest-and-matches-the-catalog|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-instance-user-data-executes-and-the-guest-reports-the-nonce|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-launch-with-a-private-ip-already-in-use-rejected|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-security-group-ingress-decides-whether-the-instance-answers|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-snapshot-restores-the-exact-bytes-written-to-the-source-volume|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: ec2.ec2-stop-start-preserves-root-volume-bytes-and-reassigns-the-public-ip|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: ec2.hibernate-restores-the-running-process-and-a-plain-stop-does-not|_poll#3|Timeout|time.sleep(interval_s)
Issue-Key: ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Key: ec2.idvdoc-signature-verifies-against-the-aws-public-certificate|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ec2.imdshop-container-on-the-default-bridge-blocked-at-hop-limit-1-and-served-at-2|_run_guest#10|Timeout|time.sleep(5)
Issue-Key: ec2.multiattach-detaching-one-guest-leaves-the-other-attachment-live-and-its-bytes-intact|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Key: ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point|_poll#14|Timeout|time.sleep(interval_s)
Issue-Key: ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd|_poll#16|Timeout|time.sleep(interval_s)
Issue-Key: ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot|_poll#16|AssertionError|raise AssertionError(
Issue-Key: ec2.secip-assigning-more-private-ips-than-the-instance-type-permits-rejected|_poll#19|Timeout|time.sleep(interval_s)
Issue-Key: ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address|_poll#19|AssertionError|raise AssertionError(
Issue-Key: ec2.secip-secondary-private-ip-is-reachable-from-a-peer-only-when-the-eni-holds-it|_poll#14|Timeout|time.sleep(interval_s)
Issue-Key: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop|_poll#12|Timeout|time.sleep(interval_s)
Issue-Key: ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable|_poll#12|Timeout|time.sleep(interval_s)
Issue-Key: ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance|_poll#12|Timeout|time.sleep(interval_s)
Issue-Key: ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic|_poll#6|Timeout|time.sleep(interval_s)
Issue-Key: ec2.asghc-launch-template-round-trips-imdsv2-user-data-and-its-omissions|run#11|AssertionError|assert got == want, f"declared values did not round-trip: got {got!r}, want {want!r}"
Issue-Commit: 620c90c23a321396910fd66a692ef2fcecf33f7f

# EC2: no instance ever presents a guest, so 38 guest-observable checks never converge

> _Note: this report is the guest-observable half of
> `20261107T075601Z-ec2-createvpc-from-an-ipam-pool-is-a.md`, split out when that issue's five
> other causes (the IPAM refusals and representations, the StateTransitionReason format, the
> termination-protection message and the cross-VPC security-group reference) were fixed. It
> carries that issue's cause 6 and every Issue-Key that cause owns, unchanged, so none of them
> is reported as working. The code base may have drifted from commit
> `620c90c23a321396910fd66a692ef2fcecf33f7f`._

## Operation
RunInstances

## Description
An EC2 instance never presents a guest or converges to the state a waiter needs, so 37 checks
spin to the 60s budget and one launch-template round-trip check fails its comparison.

AWS: a launched instance boots, runs its UserData, serves IMDS (hop limit and token TTL
enforced), registers an SSM agent that comes Online, exposes its attached volumes as block
devices whose bytes survive detach/reattach, stop/start and snapshot/restore, reports
instance-store volumes as NVMe namespaces, answers on its security-group-admitted ports,
honours source/dest check, and advances through stop/start/terminate — and the identity
document, hibernate-resume, capacity-reservation consumption and shutdown-behavior paths each
settle to an observable state (recorded behavior). The launch-template check declares IMDSv2
metadata options and UserData (and leaves other members out) and expects the declared values —
and only those — to round-trip.

Emulator: none of this converges: every guest check ends "Check timed out after 60.0s" (two of
them as an explicit assertion that the SSM agent never came Online within the wait), and the
launch-template check fails with "declared values did not round-trip".

Why: the entire guest-observable half of EC2 — UserData execution, IMDS, EBS/instance-store
data, in-guest networking, shutdown behaviour, identity documents — is unobservable, and a
client cannot distinguish "the behaviour is wrong" from "nothing ever happened".

Fix: the EC2 instance runtime and state convergence (guest boot/UserData, IMDS service, volume
data plane, instance state machine) behind these waits. Every check in the trace list needs a
guest to exist: SSH/SSM into a booted instance, in-guest binaries (an x86-64 payload, `fio`,
`resize2fs`, `growpart`), IMDS answered over the guest's own network stack at a hop limit, and
block devices a guest kernel enumerates. That needs a runtime this emulator does not have
today (no container or external-process runtime is available to it, and no in-process engine
can execute a Linux userland), so this report stays filed until one exists; it is NOT a
fidelity defect that can be closed by changing a handler's answer.

The launch-template key is the one check of this set that may not need a guest: mainline's
`DescribeLaunchTemplateVersions` projection does carry `MetadataOptions` and `UserData`
(`services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_launch_template_versions.rs`,
`response_launch_template_data`), and an Auto Scaling launch resolves the version's option sets
through Amazon EC2 itself
(`services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/state/ec2_instances.rs`,
`LaunchParameters::detail`), so which declared value does not round-trip is not identifiable
from this report alone — the member that diverges has to come from a replay of
`ec2.asghc-launch-template-round-trips-imdsv2-user-data-and-its-omissions`.

## Evidence
From the behavioral replay:
- the 37 guest checks: "Error: Check timed out after 60.0s"
  (nvme-instance-store-serial-to-device-mapping... and secip-hot-attached-second-eni... fail as
  "the SSM agent on i-... coming Online not met within ~47s").
- asghc-launch-template-round-trips-imdsv2-user-data-and-its-omissions [mismatch]: "declared
  values did not round-trip: got ..., want ...".

## Steps to Reproduce
Replay the behavioral suite and re-run every check in trace_ids: launch an instance and wait on
any guest-visible effect (UserData nonce, SSM agent Online, volume bytes, in-guest
reachability); and declare IMDSv2 metadata options and UserData on a launch template an Auto
Scaling group launches from, then read the declared values back.
