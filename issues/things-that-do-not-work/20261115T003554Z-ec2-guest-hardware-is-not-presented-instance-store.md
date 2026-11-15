Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.ec2-instance-store-bytes-vanish-across-stop-start-while-ebs-bytes-persist ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot ec2.cpuopts-threads-per-core-one-observed-in-guest ec2.cpuopts-topology-survives-a-stop-start-cycle ec2.arm64-t4g-micro-guest-reports-aarch64 ec2.arm64-x86-64-binary-execution-in-the-guest-rejected ec2.arm64-x86-64-payload-runs-on-t3-micro-and-is-rejected-on-t4g-micro ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address ec2.hibernate-guest-process-and-its-in-memory-value-survive-the-resume
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.ec2-instance-store-bytes-vanish-across-stop-start-while-ebs-bytes-persist|_run_guest#30|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point|_guest_view#15|AssertionError|assert disks and root.get("pk") and boot, (
Issue-Key: ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd|_guest_view#18|AssertionError|assert disks and root.get("pk") and boot, (
Issue-Key: ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot|_guest_view#20|AssertionError|assert disks and root.get("pk") and boot, (
Issue-Key: ec2.cpuopts-threads-per-core-one-observed-in-guest|_guest_topology#23|AssertionError|assert not missing, (
Issue-Key: ec2.cpuopts-topology-survives-a-stop-start-cycle|_guest_topology#20|AssertionError|assert not missing, (
Issue-Key: ec2.arm64-t4g-micro-guest-reports-aarch64|run#23|AssertionError|assert _marker(out, "GUEST-UNAME-M") == "aarch64", (
Issue-Key: ec2.arm64-x86-64-binary-execution-in-the-guest-rejected|run#20|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.arm64-x86-64-payload-runs-on-t3-micro-and-is-rejected-on-t4g-micro|run#23|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address|run#28|AssertionError|assert before["sif"] == "none", (
Issue-Key: ec2.hibernate-guest-process-and-its-in-memory-value-survive-the-resume|_run_guest#29|AssertionError|assert inv["Status"] == "Success", (
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# EC2 guest hardware is not presented: instance-store NVMe namespaces, CPU topology under /sys, aarch64 on Graviton types, hot-attached ENIs and hibernation RAM all absent from the guest

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Five further net-new root causes in the EC2 data plane, each a different piece of guest-visible hardware/state. They are separate causes with distinct failure signatures; closing one does not close the others. An open issue 20261113T123420Z-ec2-data-plane-a-launched-guest-never-runs.md describes the same family but does not carry these checks' replay keys.

Cause 1 of 5: instance-store (local NVMe) namespaces never appear in the guest and the guest's block-device inventory is empty.
AWS: on an instance type with local NVMe instance store, the guest exposes the instance-store namespaces as /dev/nvme*n* with an Amazon EC2 NVMe serial that maps back to the instance-store device, while a Xen-style EBS root stays reachable as /dev/xvda; instance-store bytes vanish across a stop/start while EBS bytes persist, and an instance-store device path is refused as an EBS attachment point (recorded behavior only).
Emulator: the guest prints an empty inventory — "SIM2REAL-ROOT src= real= pk=", "SIM2REAL-COMPAT path=/dev/xvd* real=", "SIM2REAL-COMPAT path=/dev/sd* real=" with no namespaces at all — and the two-filesystem provisioning probe fails rc=1 with 'NO-LOCAL-NVME'.
Why: a client cannot distinguish ephemeral from durable storage, cannot find its instance-store devices by serial, and loses the refusal that stops it attaching an EBS volume onto an instance-store device path.
Fix: the instance data plane's block-device presentation for instance-store volumes, and the guest device inventory it exposes.
Traces: ec2.ec2-instance-store-bytes-vanish-across-stop-start-while-ebs-bytes-persist; ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point; ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd; ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot.

Cause 2 of 5: the guest exposes no CPU topology — /sys/devices/system/cpu/online and /sys/devices/system/cpu/cpu0/topology/thread_siblings_list do not exist and nproc/lscpu report nothing.
AWS: an instance launched with CpuOptions (Smithy: CpuOptionsRequest.CoreCount, CpuOptionsRequest.ThreadsPerCore — "To disable multithreading for the instance, specify a value of 1") presents exactly that topology to the guest, readable via nproc, /sys/devices/system/cpu/online, thread_siblings_list and lscpu, and the topology survives a stop/start (recorded behavior only for the guest-side reading).
Emulator: "the guest did not report ['LSCPU-CPS','LSCPU-CPUS','LSCPU-TPC','NPROC','ONLINE','PROCESSORS','SIBLINGS']" with stdout showing "cat: /sys/devices/system/cpu/online: No such file or directory" and "cat: /sys/devices/system/cpu/cpu0/topology/thread_siblings_list: No such file or directory".
Why: licence-sensitive and thread-pinned workloads size themselves from the guest's CPU topology; with no topology they mis-size, and a customer who paid to disable multithreading cannot confirm it took effect.
Fix: the guest's virtual CPU presentation (the /sys CPU topology the instance's CpuOptions must drive).
Traces: ec2.cpuopts-threads-per-core-one-observed-in-guest; ec2.cpuopts-topology-survives-a-stop-start-cycle.

Cause 3 of 5: a Graviton (arm64) instance type does not present an arm64 machine to the guest — `uname -m` is empty and no arm64 binary environment exists.
AWS: an arm64 AMI on an arm64 type (t4g.*) boots a guest whose kernel reports machine 'aarch64', whose ELF class/e_machine indicate AArch64, and on which an x86-64 payload is refused by the loader while the native payload runs; the mirrored x86-64 host runs the x86-64 payload (recorded behavior only).
Emulator: the arm64 guest reports nothing — "the kernel on a t4g.micro launched from an arm64 AMI must report machine 'aarch64'; the guest reported ''" with every GUEST-* field empty; and the cross-architecture payload probes end 'Failed' with the payload file missing, so the architecture boundary is never exercised.
Why: architecture is the one property a client cannot afford to guess — an emulator that reports nothing (and, per a separate already-filed cause, also accepts mismatched AMI/type launches) lets a build pipeline ship binaries that cannot run.
Fix: the instance data plane's guest architecture presentation for arm64 instance types.
Traces: ec2.arm64-t4g-micro-guest-reports-aarch64; ec2.arm64-x86-64-binary-execution-in-the-guest-rejected; ec2.arm64-x86-64-payload-runs-on-t3-micro-and-is-rejected-on-t4g-micro.

Cause 4 of 5: a hot-attached secondary ENI never appears inside the guest, and the guest reports no interfaces at all.
AWS: AttachNetworkInterface on a running instance makes the new interface visible in the guest as an additional NIC with its own MAC and private address, distinguishable from the primary interface (recorded behavior only for the guest-side view).
Emulator: before the attach the guest already reports an empty interface set and an empty MAC list — "the guest already has an interface for MAC '<mac>' BEFORE the attach ('') ... it reported {'pif': '', 'paddr': '', 'sif': '', 'saddr': '', 'macs': ''}" — so neither the pre-state nor the attach is observable. The MAC in the message is the per-run address of the ENI being attached.
Why: multi-homed workloads (appliances, secondary-IP services, per-interface routing) cannot configure the interface they just attached.
Fix: the network-interface attachment data plane — presenting an attached ENI to the guest and exposing the guest's interface/MAC inventory.
Traces: ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address.

Cause 5 of 5: hibernation does not preserve guest memory — the in-memory holder process never even starts.
AWS: StopInstances with Hibernate=true on a hibernation-configured instance writes RAM to the encrypted root volume and the resume restores the running process and its in-memory value, whereas a plain stop does not (recorded behavior only).
Emulator: the probe that starts the in-memory holder fails before hibernation is reached — "starting the in-memory holder process: the guest script was 'Failed' ('Failed'); rc=1 stdout='HOLDER-NEVER-STARTED'".
Why: hibernation's entire value is that the process and its memory survive; a client that hibernates loses state while the control plane reports a successful stop/start.
Fix: the StopInstances hibernate path and the guest memory state it must snapshot and restore.
Traces: ec2.hibernate-guest-process-and-its-in-memory-value-survive-the-resume.

## Evidence
Cause 1 — Expected: guest lists /dev/nvme*n* instance-store namespaces with EC2 NVMe serials and an /dev/xvda EBS root. Actual: AssertionError: the guest did not report a usable block-device inventory; its stdout was 'SIM2REAL-ROOT src= real= pk=\nSIM2REAL-COMPAT path=/dev/xvd* real=\nSIM2REAL-COMPAT path=/dev/sd* real=\nSIM2REAL-BOOT id=...'; and AssertionError: provisioning the two filesystems: the guest script was 'Failed' ('Failed'); rc=1 stdout='NO-LOCAL-NVME'.

Cause 2 — Expected: nproc/lscpu and /sys/devices/system/cpu/{online,cpu0/topology/thread_siblings_list} report the launched CpuOptions topology. Actual: AssertionError: the guest did not report ['LSCPU-CPS','LSCPU-CPUS','LSCPU-TPC','NPROC','ONLINE','PROCESSORS','SIBLINGS']; stdout contains 'cat: /sys/devices/system/cpu/online: No such file or directory' and 'cat: /sys/devices/system/cpu/cpu0/topology/thread_siblings_list: No such file or directory'.

Cause 3 — Expected: `uname -m` == 'aarch64' on a t4g.micro from an arm64 AMI; an x86-64 payload is refused by the loader there and runs on t3.micro. Actual: AssertionError: the kernel on a t4g.micro launched from an arm64 AMI must report machine 'aarch64'; the guest reported '' — Full stdout: 'GUEST-UNAME-M \nGUEST-LONGBIT \nGUEST-SH-EMACHINE \nGUEST-SH-ELFCLASS \nGUEST-CPU-IMPL \nGUEST-X86-FLAG-LINES \nGUEST-X86-LOADERS 0\nGUEST-IMDS-TYPE \nGUEST-IMDS-IID \n'; the payload probes end 'Failed' with 'sha256sum: x86only: No such file or directory'.

Cause 4 — Expected: the guest shows one interface before AttachNetworkInterface and two after, the new one carrying the ENI's MAC and private IP. Actual: AssertionError: the guest already has an interface for MAC '02:00:00:00:00:b0' BEFORE the attach ('') — nothing later could be attributed to the attach; it reported {'pif': '', 'paddr': '', 'sif': '', 'saddr': '', 'macs': ''}.

Cause 5 — Expected: the holder process and its in-memory value survive StopInstances(Hibernate=true) + start. Actual: AssertionError: starting the in-memory holder process: the guest script was 'Failed' ('Failed'); rc=1 stdout='HOLDER-NEVER-STARTED'.

## Steps to Reproduce
Cause 1: RunInstances an instance type with local NVMe instance store plus an EBS root; in the guest list /dev/nvme*, read the NVMe serials, write bytes to an instance-store device and to an EBS device, stop/start, and re-read. AWS shows the namespaces and loses only the instance-store bytes; the emulator shows no devices (NO-LOCAL-NVME).

Cause 2: RunInstances with CpuOptions={'CoreCount': N, 'ThreadsPerCore': 1}; in the guest read nproc, /sys/devices/system/cpu/online, /sys/devices/system/cpu/cpu0/topology/thread_siblings_list and lscpu; stop/start and re-read. AWS reports the requested topology both times; the emulator has no /sys CPU tree.

Cause 3: RunInstances a t4g.micro from an arm64 AMI; in the guest run `uname -m` (expect 'aarch64') and try to execute an x86-64 ELF (expect loader refusal); repeat the payload on a t3.micro (expect success).

Cause 4: RunInstances an instance, CreateNetworkInterface in the same subnet, AttachNetworkInterface at DeviceIndex 1 on the running instance, and list interfaces/MACs/addresses in the guest before and after.

Cause 5: RunInstances with HibernationOptions={'Configured': True} and a root volume large enough for RAM; start a process in the guest holding a known value in memory; StopInstances(Hibernate=True); start the instance and confirm the process and its value are still there, and that a plain stop/start loses them.
