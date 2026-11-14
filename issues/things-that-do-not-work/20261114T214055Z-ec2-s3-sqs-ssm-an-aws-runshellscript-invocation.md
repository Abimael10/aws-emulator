Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__s3__sqs__ssm.instance-job-writes-objects-and-every-s3-event-lands-in-sqs-with-matching-etag-size-sequencer
Issue-Key: ec2__s3__sqs__ssm.instance-job-writes-objects-and-every-s3-event-lands-in-sqs-with-matching-etag-size-sequencer|_json_line#7|JSONDecodeError|return json.loads(lines[-1])
Issue-Commit: 54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1

# ec2__s3__sqs__ssm: an AWS-RunShellScript invocation's StandardOutputContent does not end in the JSON the shipped guest program prints, so the S3-event scenario never reaches a verdict

> _Note: this issue was identified against the code base at commit `54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
1 net-new root cause in SSM Run Command's guest execution, grounded in the one failing check under the `ec2__s3__sqs__ssm` heading that is neither ALREADY FILED nor OWNED.

Nearest open issue, which does NOT carry this failure's replay key: 20261114T172248Z-ec2-s3-sqs-ssm-aws-runshellscript-invocations-that.md. That issue describes an invocation that ends Failed/ResponseCode 127 with `StandardOutputContent=''` (its grounding assertion was `the guest reported no stdout`). That is no longer what happens for this check: the invocation now returns a NON-EMPTY StandardOutputContent — the check's `assert lines` passes — and the failure has moved one step later, into parsing that output. So a fix to that issue closes the empty-stdout cause, not this one; the residue below is narrower and must be closed on its own.

Cause 1 of 1: the final line of a Run Command invocation's StandardOutputContent is not the JSON document the guest program wrote.
  AWS: an `AWS-RunShellScript` invocation on an Online managed node runs the document's `commands` as a real shell script in the instance's guest, and `GetCommandInvocation` reports exactly what that script wrote to stdout in `StandardOutputContent`, with `Status` `Success` and `ResponseCode` 0 when it exits zero. The script this check ships is `set -uo pipefail` / `aws s3api get-object --bucket <bucket> --key in/source /tmp/src` / `python3 - <<'GENEOF' … GENEOF` (which writes /tmp/manifest.json) / a `for` loop of `aws s3api put-object` / `cat /tmp/manifest.json`. On AWS the last non-blank stdout line is therefore the single-line JSON object that `json.dump` wrote — the manifest naming `_src_sha256` and one entry per `out/part-NN` key with its size, sha256 and md5. Smithy (ssm): GetCommandInvocation output members `Status` (enum CommandInvocationStatus), `StatusDetails` (@length 0..100), `ResponseCode` (integer, default 0), `StandardOutputContent` (@length 0..24000), `StandardErrorContent` (@length 0..8000). Whether a shipped script succeeds is the GUEST's outcome, not an API refusal, so no modelled error is involved; the exact bytes a shell writes are recorded behavior only, but that the stdout a client reads back IS the script's own stdout is the requirement.
  Emulator: the invocation reaches a terminal status and `StandardOutputContent` is non-empty (the check's `assert lines, 'the guest reported no stdout'` no longer fires), but its last non-blank line is not JSON: `json.loads` on it raises `json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)`, i.e. the line's first character cannot begin a JSON value. The manifest the program was supposed to write and `cat` back is therefore not what the client reads; the guest wrote something else. Per services/service-com-amazonaws-ssm-amazonssm/src/state/guest_execution.rs the script is performed in the bounded in-process shell session reached through `ComAmazonawsEc2Amazonec2State::run_in_instance_guest`, which the module itself documents as having "no process, no socket and no host path in it" and where "a command the session provides no program for answers the status a shell answers for a utility it could not find" — so the here-document redirection, the `python3` interpreter and the `cat` of the written file are not all being honoured, and whatever that session echoes instead becomes the invocation's stdout.
  Why: a client that drives an in-VPC agent by shipping it a program — the only way to observe or act on something reachable solely from inside the VPC, and what this whole scenario is built on — reads back output that is not its program's. Every downstream claim is unreachable: that 20 guest-written objects each produce exactly one s3:ObjectCreated:Put record carrying the eTag, size and a hex sequencer matching the bytes written, that an overwrite and a delete raise that sequencer strictly, that an ObjectRemoved record omits size and eTag, and that a queue subscribed to a non-matching prefix receives nothing. None of it is graded, however faithful the S3 and SQS planes are.
  Fix: the guest-execution seam the invocation runs in — the in-process shell session reached through `ComAmazonawsEc2Amazonec2State::run_in_instance_guest` and the program environment it installs, read by SSM through services/service-com-amazonaws-ssm-amazonssm/src/state/guest_execution.rs (`perform_in_guest`), plus the invocation-result projection that fills `StandardOutputContent` for GetCommandInvocation. It needs here-document redirection to a writable /tmp path and reading it back, a `python3` able to run a script that talks to the emulator's S3 endpoint, the `aws` CLI the launched AL2023 image ships, and — critically — stdout that contains only what the script itself wrote.
  Traces: ec2__s3__sqs__ssm.instance-job-writes-objects-and-every-s3-event-lands-in-sqs-with-matching-etag-size-sequencer (verdict: error). Check helper `_json_line(inv)`: `lines = [ln for ln in inv['StandardOutputContent'].splitlines() if ln.strip()]` then `json.loads(lines[-1])`.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout. Report heading `ec2__s3__sqs__ssm`: 1/8 (12%) passed, 7 failed; six of the seven are OWNED by 20261114T172248Z-ec2-s3-sqs-ssm-aws-runshellscript-invocations-that.md, and this one is not owned by any open issue.

[error] ec2__s3__sqs__ssm.instance-job-writes-objects-and-every-s3-event-lands-in-sqs-with-matching-etag-size-sequencer
  File "decoder.py", line 338, in decode
    obj, end = self.raw_decode(s, idx=_w(s, 0).end())
  File "decoder.py", line 356, in raw_decode
    raise JSONDecodeError("Expecting value", s, err.value) from None
  json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)

Everything up to the invocation succeeds: the VPC, private subnet, route table, two security groups, three interface endpoints (ssm, ssmmessages, ec2messages) and the S3 gateway endpoint are created; the bucket, its tagging, two SQS queues and a three-configuration PutBucketNotificationConfiguration (out/ created, out/ removed, in/ created) are accepted; the `in/source` object is PUT; an instance role + profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore plus a scoped s3 policy is created; RunInstances with that profile succeeds; DescribeInstanceInformation reports PingStatus=Online; SendCommand(DocumentName="AWS-RunShellScript", TimeoutSeconds=600) is accepted; GetCommandInvocation reaches a terminal status.

Expected (AWS): StandardOutputContent whose last non-blank line is the one-line JSON manifest the shipped program printed, e.g. a JSON object with a `_src_sha256` key and 20 `out/part-NN` entries each carrying `size`, `sha256` and `md5` — followed by Status='Success' and ResponseCode=0.
Actual (emulator): StandardOutputContent is non-empty but its last non-blank line is not a JSON value at all; json.loads fails at char 0. The instance id, bucket name, command id and the digests themselves are per-run values and are not part of the requirement — the requirement is that the stdout read back is the script's own stdout.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 and re-run the check id in trace_ids (report heading `ec2__s3__sqs__ssm`).
3. Or directly, against the emulator:
   a. Create a VPC and a subnet; create an S3 gateway VPC endpoint on the subnet's route table; create an S3 bucket.
   b. Create an IAM role + instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore, read the AMI id from the public parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64, and RunInstances a t3.micro with that profile into the subnet.
   c. Poll ssm DescribeInstanceInformation until the node reports PingStatus=Online.
   d. ssm SendCommand DocumentName=AWS-RunShellScript, InstanceIds=[<id>], Parameters={"commands": ["set -uo pipefail\npython3 - <<'EOF'\nimport json\nwith open('/tmp/manifest.json','w') as fh: json.dump({'ok': True}, fh)\nEOF\ncat /tmp/manifest.json\n"]}.
   e. Poll ssm GetCommandInvocation(CommandId=…, InstanceId=…) to a terminal status and read StandardOutputContent.
   Expected: Status=Success, ResponseCode=0, and the last non-blank line of StandardOutputContent parses as JSON ({"ok": true}).
   Actual: StandardOutputContent is non-empty but its last non-blank line is not a JSON value — json.loads raises 'Expecting value: line 1 column 1 (char 0)'.
