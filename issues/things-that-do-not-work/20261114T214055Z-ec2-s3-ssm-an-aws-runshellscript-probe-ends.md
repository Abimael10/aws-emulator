Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__s3__ssm.sep-network-truth-vpc-endpoint-policy-deny-blocks-an-iam-allowed-s3-write-rejected
Issue-Key: ec2__s3__ssm.sep-network-truth-vpc-endpoint-policy-deny-blocks-an-iam-allowed-s3-write-rejected|_run#6|AssertionError|assert inv["Status"] == "Success" and inv["ResponseCode"] == 0, (
Issue-Commit: 54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1

# ec2__s3__ssm: an AWS-RunShellScript probe ends Failed with ResponseCode 127 and an empty StandardErrorContent, so no in-VPC S3 call is ever made and the VPC-endpoint-policy verdict is unreachable

> _Note: this issue was identified against the code base at commit `54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
2 net-new root causes, both in SSM Run Command's guest execution, grounded in the one failing check under the `ec2__s3__ssm` heading (which is neither ALREADY FILED nor OWNED by any open issue).

This scenario's only observation point is a managed node inside the VPC: a gateway-endpoint policy can only be shown to deny or allow a write by making that write from inside the VPC, which the check does by shipping an `aws s3api` + `python3 -c` script with `AWS-RunShellScript`. Nearest open issue, which carries neither this check id nor its replay key: 20261114T172248Z-ec2-s3-sqs-ssm-aws-runshellscript-invocations-that.md (same two causes, but grounded on and keyed to the seven `ec2__s3__sqs__ssm` checks). Closing that issue does not close this one.

Cause 1 of 2: a Run Command script that runs `aws`, `python3` or `printf` with file redirection is reported Failed/127 instead of being run.
  AWS: an `AWS-RunShellScript` invocation on an Online managed node runs the document's `commands` as a real shell script in the instance's guest. A script of the form `set +e` / `printf '%s' '<payload>' > /tmp/sim2real_payload` / `aws s3api put-object --bucket <b> --key <k> --body /tmp/sim2real_payload --region <r> > /tmp/sim2real_o 2> /tmp/sim2real_e` / `RC=$?` / `python3 -c '…print(json.dumps({...}))' $RC` therefore writes the payload file, runs the AWS CLI the AMI ships (the check launches the image named by the public parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64, which ships python3 and the AWS CLI), and ends on the exit status of the last command — so `GetCommandInvocation` reports `Status` `Success`, `ResponseCode` 0 and the single JSON line on stdout, with the CLI's own stderr captured in /tmp/sim2real_e and echoed inside that JSON. Smithy (ssm): GetCommandInvocation output members `Status` (enum CommandInvocationStatus), `StatusDetails` (@length 0..100), `ResponseCode` (integer, default 0), `StandardOutputContent` (@length 0..24000), `StandardErrorContent` (@length 0..8000). A script's success or failure is the guest's, not an API refusal, so no modelled error is involved.
  Emulator: the invocation settles `Status` `'Failed'` with `ResponseCode` 127 — the status a shell answers for a program it could not find — and `StandardErrorContent` `''`. Per services/service-com-amazonaws-ssm-amazonssm/src/state/guest_execution.rs the script is performed in the bounded in-process shell session reached through `ComAmazonawsEc2Amazonec2State::run_in_instance_guest`, documented in that module as having "no process, no socket and no host path in it" and where "a command the session provides no program for answers the status a shell answers for a utility it could not find". `aws` and `python3` are not provided, so the script dies on its first such line and no S3 request is ever issued from inside the VPC.
  Why: a client that drives an in-VPC agent by shipping it a program gets a failed invocation instead of its program's output and side effects. Nothing this scenario grades is observable: that a gateway-endpoint policy denying `s3:PutObject` refuses an in-VPC write that the instance's IAM role allows (with an exact AccessDenied code and message and no object left behind), that reads still succeed under the same policy, and that resetting the endpoint policy makes the same write succeed — none of it is measured, however faithful the S3 and VPC-endpoint control planes are.
  Fix: the guest-execution seam the invocation runs in — the in-process shell session reached through `ComAmazonawsEc2Amazonec2State::run_in_instance_guest` and the program environment it installs, read by SSM through services/service-com-amazonaws-ssm-amazonssm/src/state/guest_execution.rs (`perform_in_guest`). It needs the shell features such a probe uses (`printf`, `rm`, `cat`, `> file` and `2> file` redirection under /tmp, `$?`), a `python3` interpreter, and an `aws` CLI whose requests leave the guest through the instance's network so a VPC endpoint policy can actually decide them.
  Traces: ec2__s3__ssm.sep-network-truth-vpc-endpoint-policy-deny-blocks-an-iam-allowed-s3-write-rejected — `AssertionError: the control command itself failed on i-…: status='Failed' rc=127 stderr=''`, raised by the check's `_run()` at `assert inv["Status"] == "Success" and inv["ResponseCode"] == 0`.

Cause 2 of 2: a Failed invocation reports an empty StandardErrorContent, so the failure carries no diagnostic.
  AWS: when a RunShellScript script ends non-zero the agent reports the script's own stderr in `StandardErrorContent` alongside the non-zero `ResponseCode`. For a program the shell cannot find that stderr is the shell's own diagnostic for the failing line — fixed text of the form `<program>: command not found`, whose only variable part is the program name taken from the script line that failed (the exact per-shell wording is recorded behavior only; the requirement is that a non-empty diagnostic accompanies a Failed status with a non-zero code).
  Emulator: `Status` `'Failed'` and `ResponseCode` 127 are reported while `StandardErrorContent` is `''` — the reason the guest failed is serialized nowhere a client can read it.
  Why: a client cannot distinguish a missing interpreter from a syntax error, a permission failure or a killed probe; all of them read as an empty Failed. Automation that branches on `StandardErrorContent` to retry, report or classify has nothing to branch on, and this check's own diagnostic degrades to `stderr=''`.
  Fix: the invocation-result projection in SSM's guest execution and the GetCommandInvocation serializer — carry the session's stderr stream into `StandardErrorContent` (keeping `StatusDetails` consistent) instead of dropping it when the script ends non-zero.
  Traces: the same check id; its assertion message ends in `stderr=''`.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout. Report heading `ec2__s3__ssm`, 1 failed check, not marked ALREADY FILED and not OWNED by any open issue.

[mismatch] ec2__s3__ssm.sep-network-truth-vpc-endpoint-policy-deny-blocks-an-iam-allowed-s3-write-rejected
  File "check_…_sep_network_truth_vpc_endpoint_policy_deny_blocks_an_iam_allowed_s3_write_rejected.py", line 123, in _guest_s3
    res = _run(ctx, ssm, st["client"], script)
  File "…", line 546, in _run
    assert inv["Status"] == "Success" and inv["ResponseCode"] == 0, (
  AssertionError: the control command itself failed on i-00000000000000001: status='Failed' rc=127 stderr=''

Everything up to the invocation succeeds: the bucket is created, the VPC/subnet/security group and the S3 gateway endpoint are created, an IAM role + instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore and an inline policy allowing s3:PutObject/s3:GetObject on arn:aws:s3:::<bucket>/* is created, RunInstances with that profile succeeds, DescribeInstanceInformation reports PingStatus=Online, and SendCommand(DocumentName="AWS-RunShellScript", Parameters={"commands": [<one multi-line script>]}, TimeoutSeconds=600) is accepted and reaches a terminal status.

The script sent (control leg, before any endpoint policy is applied):
  set +e
  printf '%s' '<payload>' > /tmp/sim2real_payload
  aws s3api put-object --bucket <bucket> --key <key> --body /tmp/sim2real_payload --region <region> > /tmp/sim2real_o 2> /tmp/sim2real_e
  RC=$?
  python3 -c 'import json,sys;print(json.dumps({"rc":int(sys.argv[1]),"out":open("/tmp/sim2real_o").read()[-600:],"err":open("/tmp/sim2real_e").read()[-600:]}))' $RC

Expected (AWS): Status='Success', ResponseCode=0, StandardOutputContent ending in the one JSON line the probe prints (carrying the CLI's rc, stdout and stderr), and the object present in the bucket.
Actual (emulator): Status='Failed', ResponseCode=127, StandardErrorContent='' — the script never ran (cause 1) and the failure carries no diagnostic (cause 2). The instance id `i-00000000000000001`, bucket name and command id are per-run values, not part of the requirement.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 and re-run the check id in trace_ids (report heading `ec2__s3__ssm`).
3. Or directly, against the emulator:
   a. Create a bucket, a VPC, a subnet with an internet gateway, a security group, and an S3 gateway VPC endpoint on the subnet's route table.
   b. Create an IAM role + instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore plus an inline policy allowing s3:PutObject/s3:GetObject on arn:aws:s3:::<bucket>/*; read the AMI id from /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 and RunInstances a t3.micro with that profile into the subnet.
   c. Poll ssm DescribeInstanceInformation until PingStatus=Online.
   d. ssm SendCommand DocumentName=AWS-RunShellScript with commands=["set +e\nprintf '%s' 'hello' > /tmp/p\naws s3api put-object --bucket <bucket> --key k --body /tmp/p --region <region> > /tmp/o 2> /tmp/e\nRC=$?\npython3 -c 'import sys;print(sys.argv[1])' $RC\n"].
   e. Poll ssm GetCommandInvocation to a terminal status.
   Expected: Status=Success, ResponseCode=0, StandardOutputContent ending in '0', and HeadObject on <bucket>/k succeeding.
   Actual: Status=Failed, ResponseCode=127, StandardErrorContent='' and no object written.
4. Cause 2 alone, needing no interpreter: send commands=["definitely-not-a-program"] and read GetCommandInvocation — expect `definitely-not-a-program: command not found` in StandardErrorContent with ResponseCode 127; actual is ResponseCode 127 with StandardErrorContent=''.
