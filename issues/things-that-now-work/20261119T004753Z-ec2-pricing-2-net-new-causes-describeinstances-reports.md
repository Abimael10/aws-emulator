Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__pricing.ecprice-cli-trajectory-matches-sdk-estimate ec2__pricing.ecprice-platform-filter-partitions-windows-from-linux
Issue-Key: ec2__pricing.ecprice-cli-trajectory-matches-sdk-estimate|run#16|AssertionError|assert sorted(lines) == sorted([["t3.micro", "windows"], ["t3.nano", "None"]]), (
Issue-Key: ec2__pricing.ecprice-platform-filter-partitions-windows-from-linux|run#10|AssertionError|assert windows == win, f"platform=windows returned {windows!r}, expected {win!r}"
Issue-Commit: 6bca3e6091bb695822e734a86e830b00eaa8296d

# ec2__pricing: 2 net-new causes — DescribeInstances reports Instance.Platform as 'Windows' instead of the lower-case 'windows' (SDK and CLI text projection), and the platform=windows filter matches no running Windows instance

> _Note: this issue was identified against the code base at commit `6bca3e6091bb695822e734a86e830b00eaa8296d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
Two net-new root causes replayed under the `ec2__pricing` suite heading. Both are in how EC2 represents and matches an instance's Windows platform, which is how a cost-estimation client splits Windows instances from Linux ones before pricing them.

Related open issue: 20261103T021956Z-ec2-pricing-pricing-getproducts-matches-no-sku-for-the-standard.md. That issue owns the GetProducts empty-PriceList cause and the `ecprice-linux-instance-omits-platform-and-prices-as-linux` check, and it also recorded the `platform` filter as a hard refusal ('InvalidParameterValue', "The filter 'platform' is not one this emulator resolves"). Both of the causes below are replayed by checks this run leaves unmarked, and Cause 2 now diverges in a DIFFERENT way than that issue describes (the filter is accepted and returns an empty set rather than being refused), so it is filed here.

Cause 1 of 2: the instance Platform value is reported capitalised, so every literal comparison and every text projection of it is wrong.
AWS: Smithy — Instance.Platform targets com.amazonaws.ec2#PlatformValues and its documentation states "The platform. This value is `windows` for Windows instances; otherwise, it is empty." (The enum's symbol in the model reads `Windows`; the wire value AWS serialises, and the value the `platform` filter takes, is the lower-case literal `windows`.) For a Linux instance the member is omitted entirely, which is why `--query ...[InstanceType,Platform] --output text` renders the literal text `None` for that row. So a two-instance projection over one Windows and one Linux instance reads `<type>\twindows` and `<type>\tNone`.
Emulator: serialises `Windows`. The SDK field reads `Platform='Windows'` and the CLI text projection renders the row as `Windows`, so the rows are [[<windows-type>, 'Windows'], [<linux-type>, 'None']] where AWS gives [[<windows-type>, 'windows'], [<linux-type>, 'None']]. The Linux omission itself is correct.
Why: Platform is consumed as a literal, not as a case-insensitive label — it is the same token the `platform` filter takes and the same token pricing code maps to operatingSystem=Windows. A case-shifted value fails every `== "windows"` test, so a client classifies the Windows instance as neither Windows nor Linux and prices it against the wrong (or no) SKU. CLI/jq pipelines that compare the text projection are affected identically.
Fix: the Platform field in EC2 instance state and the DescribeInstances serializer for that member — the stored/emitted value must be the lower-case literal, and must stay absent for non-Windows instances.
Traces: ec2__pricing.ecprice-cli-trajectory-matches-sdk-estimate.

Cause 2 of 2: the DescribeInstances `platform` filter matches no instance.
AWS: Smithy — DescribeInstancesRequest.Filters documents `platform` as "The platform. To list only Windows instances, use `windows`." A DescribeInstances call with Filters=[{Name:'platform',Values:['windows']}] (plus tag/state filters) returns exactly the running Windows instances; `platform-details`=`Linux/UNIX` returns the Linux ones.
Emulator: the call now succeeds (HTTP 200) but the reservation set is EMPTY — `platform=windows` returned [] while a running Windows instance exists and is returned by the same tag+state filter set without the platform predicate.
Why: a caller cannot narrow Windows instances server-side at all. Cost and licence attribution silently sees zero Windows instances, so Windows usage is dropped from every estimate instead of being priced at the Windows rate — a wrong answer with no error to notice. Hypothesis: the predicate compares the filter value `windows` against the stored capitalised `Windows` from Cause 1; if so, fixing Cause 1 must be accompanied by a case-correct predicate, and both checks must pass.
Fix: the `platform` entry in the DescribeInstances filter table / its matching predicate, against the (corrected) Platform value in instance state.
Traces: ec2__pricing.ecprice-platform-filter-partitions-windows-from-linux.

## Evidence
Replayed failing checks under `### ec2__pricing` (neither marked ALREADY FILED nor OWNED by):

1. ec2__pricing.ecprice-cli-trajectory-matches-sdk-estimate -> AssertionError: the CLI text projection rows were [['t3.micro', 'Windows'], ['t3.nano', 'None']]
   Expected (AWS): [['t3.micro', 'windows'], ['t3.nano', 'None']]. Fixed text here is the lower-case literal `windows` for the Windows instance and the CLI's `None` rendering of the omitted member for the Linux instance; the instance types (`t3.micro`/`t3.nano`) are values from the check's own launches and are not part of the requirement.

2. ec2__pricing.ecprice-platform-filter-partitions-windows-from-linux -> AssertionError: platform=windows returned [], expected ['i-00000000000000002']
   The call is accepted (no error code) and returns an empty reservation set; the expected instance id is the Windows instance the same check launched (the literal id is per-run, the requirement is that the one running Windows instance is returned and the Linux one is not). The sibling assertion on `platform-details=Linux/UNIX` is not reached.

Contrast with the earlier recording in 20261103T021956Z-ec2-pricing-pricing-getproducts-matches-no-sku-for-the-standard.md, where the same call was refused with InvalidParameterValue: the refusal is gone, the empty result is not.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite; the two checks appear under `### ec2__pricing`, unmarked.
2. Standalone repro (one region, one AZ):
   - ec2.run_instances from a Windows AMI (e.g. Windows_Server-2022-English-Full-Base-*) and from a Linux AMI (e.g. al2023-ami-minimal-*-x86_64), both t3-class, tagged with a common probe tag; wait for both to reach `running`.
   - ec2.describe_instances(Filters=[{'Name':'tag:<probe>','Values':[<token>]},{'Name':'instance-state-name','Values':['running']}]): the Windows instance's Platform must read exactly `windows` (lower case), and the Linux instance must have no Platform member at all.
   - aws ec2 describe-instances --filters Name=tag:<probe>,Values=<token> Name=instance-state-name,Values=running --query 'Reservations[].Instances[].[InstanceType,Platform]' --output text: the Windows row's second column must be `windows`, the Linux row's must be `None`.
   - ec2.describe_instances(Filters=[... same tag/state ..., {'Name':'platform','Values':['windows']}]): must return exactly the Windows instance id, not an empty set.
   - ec2.describe_instances(Filters=[... same tag/state ..., {'Name':'platform-details','Values':['Linux/UNIX']}]): must return exactly the Linux instance id.
