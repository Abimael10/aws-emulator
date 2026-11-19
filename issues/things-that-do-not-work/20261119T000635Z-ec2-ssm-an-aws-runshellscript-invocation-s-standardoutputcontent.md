Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer
Issue-Key: ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer|_json_line#5|JSONDecodeError|return json.loads(lines[-1])
Issue-Commit: bf5f1b716fd44070c371033dd9201606434af162

# ec2__ssm: an AWS-RunShellScript invocation's StandardOutputContent is not the guest program's stdout — its last non-blank line is not the JSON the in-guest program printed

> _Note: this issue was identified against the code base at commit `bf5f1b716fd44070c371033dd9201606434af162`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
Cause 1 of 1: for an AWS-RunShellScript invocation on a managed node, GetCommandInvocation's StandardOutputContent does not carry what the guest's own program wrote to standard output. The scenario's guest script ends with a `python3` heredoc whose last statement is `print(json.dumps({...}))`, so the last non-blank line of stdout must be a single-line JSON object; the emulator serves a StandardOutputContent that is non-empty but whose last non-blank line does not begin a JSON value at all. Failing check: ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer.

AWS: recorded behavior only — the Smithy model documents GetCommandInvocation's StandardOutputContent only as the standard output of the command, with no constraint on its content. What the recording establishes is the stream discipline: on a real managed node AWS-RunShellScript runs a real shell in the instance's guest, so the commands' own standard output — and nothing else — lands in StandardOutputContent, while a shell diagnostic about a program it could not run lands in StandardErrorContent and is reflected in a non-zero ResponseCode. A guest script that ends in `print(json.dumps(d))` therefore produces a stdout whose last non-blank line parses as JSON, which is how the recorded scenario reads every leg of the security-group data plane (`{"rc": ..., "code": ..., "body": ...}` from the client, `{"local": ..., "hits": ...}` from the server).

Emulator: parsing that last non-blank line fails outright — `json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)`, raised from `json.loads(lines[-1])` after the check's own "the guest reported no stdout" guard did NOT fire. So StandardOutputContent holds at least one non-blank line and that line is not JSON. Hypothesis: the in-process guest shell has no `curl`, no `python3` and no HTTP listener program, and it writes its "command not found"-class diagnostic onto StandardOutputContent instead of StandardErrorContent, so the stream a client is told is the program's stdout is in fact emulator/shell-authored text.

Why: every client that uses a managed node as an observation point and parses its command output — the universal idiom for reading in-guest state through Run Command — gets text it cannot parse instead of its program's output, and it cannot even tell a failed invocation from a successful one by the streams, because the diagnostic is reported where success output belongs. Here it makes the whole security-group data-plane scenario unobservable: prefix-list membership edits, a prefix list beside a CIDR twin, and Reachability Analyzer's ENI_SG_RULES_MISMATCH verdict agreeing with a real TCP attempt are all read through this one stdout contract.

Fix: guest execution behind SendCommand/GetCommandInvocation in the ssm service crate (services/service-com-amazonaws-ssm-amazonssm/src/state/guest_execution.rs, with the shared shell session's program set in service_common's posix shell session and services/service-com-amazonaws-ec2-amazonec2/src/state/guest_commands.rs): route shell diagnostics to StandardErrorContent with the matching non-zero ResponseCode, keep StandardOutputContent to exactly what the invoked commands wrote to stdout, and provide the guest programs this class of script needs (`curl`, `python3` including a simple HTTP server, and a TCP client/listener) so the program's own output is what the client reads back.

Related already-filed work: open issue 20261115T000000Z-ec2-ssm-a-managed-nodes-guest-has-no-network-programs.md names the missing guest network programs for seven other ec2__ssm checks, whose failures are `ResponseCode != 0` or a missing nonce/listener marker. It does not carry this check's replay key and its failure signature is different (this one never gets to inspect rc at all — the stdout does not parse), so it does not cover this check; file and fix this stream/program defect alongside it.

Traces: ec2__ssm.sg-resolution-prefix-list-and-reachability-analyzer.

## Evidence
Behavioral suite, service heading `### ec2__ssm`, check sg-resolution-prefix-list-and-reachability-analyzer (verdict: error).

The check reads every leg through one helper:

    def _json_line(inv):
        lines = [ln for ln in inv["StandardOutputContent"].splitlines() if ln.strip()]
        assert lines, f"the guest reported no stdout: {inv.get('StatusDetails')!r} stderr={inv.get('StandardErrorContent')!r}"
        return json.loads(lines[-1])

fed by SendCommand DocumentName=AWS-RunShellScript whose script ends in

    python3 - <<'JEOF'
    import json, os
    print(json.dumps({"local": os.environ.get("LOCAL", ""), "hits": os.environ.get("HITS", "") or "0"}))
    JEOF

Replay, tail of the error:

    File "decoder.py", line 338, in decode
      obj, end = self.raw_decode(s, idx=_w(s, 0).end())
    File "decoder.py", line 356, in raw_decode
      raise JSONDecodeError("Expecting value", s, err.value) from None
    json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)

The `assert lines` guard above it did not fire, so StandardOutputContent was non-empty; the failure is that its last non-blank line is not a JSON value.

Expected (AWS): StandardOutputContent's last non-blank line is the one-line JSON object the guest's `python3` program printed (for the server leg `{"local": "<nonce>", "hits": "<n>"}`, for the client leg `{"rc": <curl rc>, "code": "<http status>", "body": "<body>"}`); any shell diagnostic appears in StandardErrorContent with a non-zero ResponseCode. The nonce, hit count and body are per-run values; the requirement is that stdout carries the guest program's output and nothing else.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Create a VPC with two subnets, an IAM role/instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore, and RunInstances one instance with that profile; poll ssm DescribeInstanceInformation until PingStatus is Online.
3. ssm SendCommand DocumentName=AWS-RunShellScript, InstanceIds=[that instance], Parameters={"commands": ["set -uo pipefail\nexport HITS=1\npython3 - <<'JEOF'\nimport json, os\nprint(json.dumps({\"hits\": os.environ.get(\"HITS\", \"\")}))\nJEOF\n"]}.
4. Poll ssm GetCommandInvocation to a terminal Status and read StandardOutputContent, StandardErrorContent and ResponseCode.
5. Observe that StandardOutputContent is non-empty but its last non-blank line is not JSON (json.loads on it raises "Expecting value: line 1 column 1 (char 0)"); AWS returns the program's JSON line on stdout, and puts any diagnostic on stderr with a matching non-zero ResponseCode.
6. Re-replay the check id in trace_ids.
