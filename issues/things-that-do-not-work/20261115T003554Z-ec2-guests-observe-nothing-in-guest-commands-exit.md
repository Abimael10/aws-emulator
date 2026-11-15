Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.diops-gp3-iops-to-size-ratio-above-the-limit-rejected-and-no-iops-reach-the-guest ec2.diops-io2-provisioned-iops-outrun-gp3-baseline-measured-by-fio-in-one-guest ec2.ltver-launch-template-aliases-resolve-to-the-version-each-guest-executes ec2.vpc-name-resolution-dhcp-black-hole-nameserver-fails-guest-lookups-while-default-resolves ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted ec2.droute-deleted-peering-target-blackholes-the-route-and-drops-traffic ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds ec2.placement-cluster-group-members-read-one-shared-zone-and-their-group-in-guest ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version ec2.idvdoc-document-reminted-and-resigned-after-a-stop-start ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document ec2.idvdoc-signature-verifies-against-the-aws-public-certificate ec2.imdshop-container-on-the-default-bridge-blocked-at-hop-limit-1-and-served-at-2 ec2.dsnapinc-changed-blocks-across-unrelated-lineages-rejected-same-lineage-lists-guest-block ec2.dsnapinc-one-changed-block-in-guest-makes-the-second-snapshot-incremental-and-the-restore-carries-it ec2.dvolgrow-capacity-added-by-modify-volume-is-unusable-until-resize2fs ec2.dvolgrow-root-volume-grows-online-and-only-growpart-reaches-the-live-root-filesystem ec2.dvolgrow-shrink-rejected-leaves-the-guest-block-device-and-its-bytes-untouched ec2.ec2-attach-volume-to-a-device-name-already-in-use-rejected ec2.multiattach-detaching-one-guest-leaves-the-other-attachment-live-and-its-bytes-intact
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.diops-gp3-iops-to-size-ratio-above-the-limit-rejected-and-no-iops-reach-the-guest|_run_guest#25|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.diops-io2-provisioned-iops-outrun-gp3-baseline-measured-by-fio-in-one-guest|_run_guest#28|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.ltver-launch-template-aliases-resolve-to-the-version-each-guest-executes|_run_guest#11|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.vpc-name-resolution-dhcp-black-hole-nameserver-fails-guest-lookups-while-default-resolves|_probe#2|AssertionError|assert inv["ResponseCode"] == 0, (
Issue-Key: ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic|_json_line#5|AssertionError|assert lines, (
Issue-Key: ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted|_json_line#6|AssertionError|assert lines, (
Issue-Key: ec2.droute-deleted-peering-target-blackholes-the-route-and-drops-traffic|_json_line#5|AssertionError|assert lines, (
Issue-Key: ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic|_payload_json#6|AssertionError|assert lines, (
Issue-Key: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop|run#17|AssertionError|assert before["imds_iid"] == instance_id, (
Issue-Key: ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable|run#20|AssertionError|assert before["imds_iid"] == instance_id, (
Issue-Key: ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance|run#17|AssertionError|assert before["imds_iid"] == instance_id, (
Issue-Key: ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source|run#22|AssertionError|assert sec.get("IID") == instance_id, (
Issue-Key: ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds|run#24|AssertionError|assert sec.get("IID") == instance_id, (
Issue-Key: ec2.placement-cluster-group-members-read-one-shared-zone-and-their-group-in-guest|run#34|AssertionError|assert recs[0].get("id") == iid, (
Issue-Key: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version|_guest_generations#23|AssertionError|assert m, f"{what}: the guest on {iid} printed {stdout!r}, not 'iid=<id> gen=<n>'"
Issue-Key: ec2.idvdoc-document-reminted-and-resigned-after-a-stop-start|_read_identity_from_guest#14|AssertionError|assert len(fields) == 4, (
Issue-Key: ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document|_guest_material#16|AssertionError|assert len(fields) == 4, (
Issue-Key: ec2.idvdoc-signature-verifies-against-the-aws-public-certificate|_guest_material#18|AssertionError|assert len(fields) == 4, (
Issue-Key: ec2.imdshop-container-on-the-default-bridge-blocked-at-hop-limit-1-and-served-at-2|_run_guest#10|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.dsnapinc-changed-blocks-across-unrelated-lineages-rejected-same-lineage-lists-guest-block|_run_guest#29|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.dsnapinc-one-changed-block-in-guest-makes-the-second-snapshot-incremental-and-the-restore-carries-it|_run_guest#31|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.dvolgrow-capacity-added-by-modify-volume-is-unusable-until-resize2fs|_run_guest#26|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.dvolgrow-root-volume-grows-online-and-only-growpart-reaches-the-live-root-filesystem|_run_guest#33|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.dvolgrow-shrink-rejected-leaves-the-guest-block-device-and-its-bytes-untouched|_run_guest#26|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.ec2-attach-volume-to-a-device-name-already-in-use-rejected|_guest_disk_sizes#19|AssertionError|assert rep and rep.get("sizes"), f"{what}: the guest reported no disks at all; stdout {out!r}"
Issue-Key: ec2.multiattach-detaching-one-guest-leaves-the-other-attachment-live-and-its-bytes-intact|_guest#26|AssertionError|assert result["Status"] == "Success" and result["ResponseCode"] == 0, (
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# EC2 guests observe nothing: in-guest commands exit 127 or return empty output, IMDS serves nothing (including the instance identity document and hop-limit enforcement), and attached EBS volumes never appear as block devices

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Six net-new root causes in the EC2 data plane. They are separate causes because each one is a different missing guest-side surface and each fails with a different signature; a fix for one does not close the others. An open issue 20261113T123420Z-ec2-data-plane-a-launched-guest-never-runs.md describes the same family but does not carry these checks' replay keys.

Cause 1 of 6: in-guest commands terminate with exit code 127 and empty stdout — the launched guest has no usable command environment, so no in-guest measurement is possible.
AWS: a running instance launched from an Amazon Linux AMI executes a shell script and reports its stdout and a real exit status (recorded behavior only; EC2's Smithy model does not describe guest execution).
Emulator: every probe ends "the guest script was 'Failed' ('Failed'); rc=127 stdout='' stderr=''".
Why: no client can verify anything about the machine it launched — device layout, DNS resolution, or which launch-template version booted.
Fix: the instance data plane's guest-execution component.
Traces: ec2.diops-gp3-iops-to-size-ratio-above-the-limit-rejected-and-no-iops-reach-the-guest; ec2.diops-io2-provisioned-iops-outrun-gp3-baseline-measured-by-fio-in-one-guest; ec2.ltver-launch-template-aliases-resolve-to-the-version-each-guest-executes; ec2.vpc-name-resolution-dhcp-black-hole-nameserver-fails-guest-lookups-while-default-resolves.

Cause 2 of 6: in-guest commands report status 'Failed' with NO stdout and NO exit code at all — the invocation itself never ran, as distinct from Cause 1's rc=127.
AWS: the invocation completes and reports the payload's own exit status and output; 'Failed' with neither output nor a response code is not a state AWS produces (recorded behavior only).
Emulator: "the guest reported no stdout: 'Failed' stderr=''" / "the guest reported no stdout at all: 'Failed' stderr=''".
Why: VPC-peering, route and source/dest-check behaviour is unobservable, so a client cannot tell a blackholed route from a working one.
Fix: the guest-execution component's invocation reporting.
Traces: ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic; ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted; ec2.droute-deleted-peering-target-blackholes-the-route-and-drops-traffic; ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic.

Cause 3 of 6: the instance metadata service is not served inside the guest — a read of /latest/meta-data/instance-id returns the empty string.
AWS: IMDS at 169.254.169.254 serves /latest/meta-data/instance-id as the launching instance's id, and /latest/api/token for IMDSv2 (recorded behavior only).
Emulator: "IMDS inside the guest must identify the instance under test: it served '', not '<instance id>'" and "the guest read instance id '' out of IMDS"; the instance id in the message is the per-run id of the instance the check launched, the requirement is that IMDS echoes whichever instance is reading it.
Why: cloud-init, the SSM agent, credential providers and every role-based client inside the guest key off IMDS; an empty answer makes all in-guest identity and credential flows impossible and makes any measurement unattributable to a machine.
Fix: the guest-facing IMDS endpoint and the per-instance metadata state behind it.
Traces: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop; ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable; ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance; ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source; ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds; ec2.placement-cluster-group-members-read-one-shared-zone-and-their-group-in-guest; ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version.

Cause 4 of 6: IMDS does not serve the signed instance identity document.
AWS: /latest/dynamic/instance-identity/document returns the JSON identity document and /latest/dynamic/instance-identity/signature (and /rsa2048, /pkcs7) return its signature, verifiable against the published AWS public certificate; the document is re-minted and re-signed across a stop/start (recorded behavior only).
Emulator: the guest's identity probe prints only the marker with no document, no signature and no token length: "the SIM2REAL-IDVDOC line must carry the document, the signature and the token length, got ['SIM2REAL-IDVDOC']".
Why: workloads that prove their own identity to an external service (mutual attestation, licence binding, node-join handshakes) have nothing to present, and a client cannot reject a foreign signature because no signature exists.
Fix: the IMDS dynamic/instance-identity endpoints and the signing material behind them.
Traces: ec2.idvdoc-document-reminted-and-resigned-after-a-stop-start; ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document; ec2.idvdoc-signature-verifies-against-the-aws-public-certificate.

Cause 5 of 6: the IMDS hop limit is never enforced and the probe path is never ready.
AWS: MetadataOptions.HttpPutResponseHopLimit (Smithy: InstanceMetadataOptionsRequest.HttpPutResponseHopLimit) sets the IP TTL budget for IMDS responses: at hop limit 1 a container on the instance's default bridge cannot reach IMDS, at 2 it can (recorded behavior only for the containerised observation).
Emulator: the probe ends "the guest script was 'Failed' ('Failed'); rc=3 stdout='SIM2REAL-ENGINE-NOT-READY'" — the container/engine path on the guest never becomes usable, so neither arm of the hop-limit boundary is observable.
Why: HttpPutResponseHopLimit is the control customers use to deny or allow container access to instance credentials; neither setting can be validated.
Fix: the guest data plane's container/runtime support and the IMDS hop-limit enforcement in the guest-facing metadata path.
Traces: ec2.imdshop-container-on-the-default-bridge-blocked-at-hop-limit-1-and-served-at-2.

Cause 6 of 6: an EBS volume whose AttachVolume succeeded never appears as a block device inside the guest.
AWS: once AttachVolume reports Attachment State 'attached' for a same-AZ volume, the guest sees a block device of the volume's size at (or symlinked from) the requested device name; its bytes survive detach/re-attach and multi-attach keeps the surviving attachment's data intact (recorded behavior only).
Emulator: guest probes report no device of the expected size, no data device, no root source and no disks at all: rc=1 with stdout 'NO-DEVICE-OF-<n>-BYTES', 'NO-DATA-DEVICE', 'NO-ROOT-SOURCE', 'MA-NODEV', and 'SIM2REAL-DISKS sizes=' (empty). The sizes and device names are per-run values; the requirement is that an attached volume is visible in the guest at all.
Why: every storage workflow that matters — snapshot/restore round trips, incremental snapshots, online volume growth, shrink refusal, multi-attach, device-name collision — becomes unverifiable, and a client that attaches a volume and writes to it loses the data silently.
Fix: the volume-attachment data plane — the component that materialises an attached volume as a guest block device, and the guest block-device inventory it exposes.
Traces: ec2.dsnapinc-changed-blocks-across-unrelated-lineages-rejected-same-lineage-lists-guest-block; ec2.dsnapinc-one-changed-block-in-guest-makes-the-second-snapshot-incremental-and-the-restore-carries-it; ec2.dvolgrow-capacity-added-by-modify-volume-is-unusable-until-resize2fs; ec2.dvolgrow-root-volume-grows-online-and-only-growpart-reaches-the-live-root-filesystem; ec2.dvolgrow-shrink-rejected-leaves-the-guest-block-device-and-its-bytes-untouched; ec2.ec2-attach-volume-to-a-device-name-already-in-use-rejected; ec2.multiattach-detaching-one-guest-leaves-the-other-attachment-live-and-its-bytes-intact.

## Evidence
Cause 1 — Expected: the in-guest script runs and prints its findings with a real exit status. Actual: AssertionError: locating the gp3 volume in the guest: the guest script was 'Failed' ('Failed'); rc=127 stdout='' stderr='' (same signature for the io2/fio, launch-template-alias and DHCP name-resolution probes).

Cause 2 — Expected: invocation completes and reports the payload's stdout and exit status. Actual: AssertionError: the guest reported no stdout: 'Failed' stderr='' / the guest reported no stdout at all: 'Failed' stderr=''.

Cause 3 — Expected: IMDS /latest/meta-data/instance-id returns the reading instance's id. Actual: AssertionError: IMDS inside the guest must identify the instance under test: it served '', not 'i-00000000000000001'; AssertionError: the guest read instance id '' out of IMDS, but this check launched 'i-00000000000000001'; AssertionError: the guest that answered for i-00000000000000001 reports instance-id ''; AssertionError: before the refresh: the guest on i-00000000000000001 printed 'iid= gen=1', not 'iid=<id> gen=<n>'.

Cause 4 — Expected: /latest/dynamic/instance-identity/document plus its signature, verifiable against the AWS public certificate. Actual: AssertionError: the SIM2REAL-IDVDOC line must carry the document, the signature and the token length, got ['SIM2REAL-IDVDOC'].

Cause 5 — Expected: a container on the default bridge is blocked at HttpPutResponseHopLimit=1 and served at 2. Actual: AssertionError: hop limit 1 on i-00000000000000001: the guest script was 'Failed' ('Failed'); rc=3 stdout='SIM2REAL-ENGINE-NOT-READY'.

Cause 6 — Expected: an attached volume is a visible guest block device of the volume's size. Actual: AssertionError: ... the guest script was 'Failed' ('Failed'); rc=1 stdout='NO-DEVICE-OF-1073741824-BYTES' / 'NO-DATA-DEVICE' / 'NO-ROOT-SOURCE'; AssertionError: the guest command ... ended Status 'Failed' ResponseCode 1; stdout='MA-NODEV'; AssertionError: the guest's disks before any attachment: the guest reported no disks at all; stdout 'SIM2REAL-DISKS sizes='. Note the same instances' console output shows a plausible boot log naming the root volume, so the control plane claims a running machine while the guest surfaces are absent.

## Steps to Reproduce
Cause 1/2: RunInstances a t3.micro from the current Amazon Linux AMI in a subnet with egress, wait for 'running', then run any shell script in the guest (e.g. `lsblk -b`, `getent hosts <name>`, `curl`): AWS returns the script's stdout and exit status; the emulator returns Failed with rc=127/empty or with no output and no rc.

Cause 3: from the same guest, read http://169.254.169.254/latest/meta-data/instance-id (with an IMDSv2 token where required): AWS returns the instance id; the emulator returns the empty string.

Cause 4: from the guest, read /latest/dynamic/instance-identity/document and /latest/dynamic/instance-identity/signature: AWS returns the JSON document and its signature; the emulator returns neither. Stop/start the instance and re-read to see the document re-minted and re-signed.

Cause 5: RunInstances with MetadataOptions.HttpPutResponseHopLimit=1, start a container on the guest's default bridge and read IMDS from it (blocked), then set the hop limit to 2 and repeat (served). The emulator's container path never becomes ready (rc=3, SIM2REAL-ENGINE-NOT-READY).

Cause 6: CreateVolume (e.g. 1 GiB) in the instance's AZ, AttachVolume at /dev/sdf, wait for Attachment State 'attached', then list block devices in the guest: AWS shows a 1 GiB device; the emulator shows none. Repeat for ModifyVolume growth, a shrink refusal, a snapshot/restore round trip, an already-in-use device name, and a multi-attach io2 volume shared by two instances.
