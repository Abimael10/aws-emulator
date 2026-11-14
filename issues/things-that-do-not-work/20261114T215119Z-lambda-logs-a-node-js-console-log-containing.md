Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event
Issue-Key: lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event|run#15|AssertionError|assert fields[1:] == [rid, "INFO", f"ML1-{m}\nML2-{m}\nML3-{m}\n"], (
Issue-Commit: fa5a827558c7852c4bea0e628f9a7809957676ef

# lambda__logs: a Node.js console.log containing embedded newlines is truncated at the first newline — only the first line reaches CloudWatch Logs, and the event loses its trailing newline

> _Note: this issue was identified against the code base at commit `fa5a827558c7852c4bea0e628f9a7809957676ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause count: 1 — one root cause must be closed before submitting.

Cause 1 of 1: the nodejs managed runtime's Text log framing is now applied, but the framed event keeps only the first line of a multi-line console.log.

AWS: Smithy (lambda) Invoke with LogType='Tail' returns the tail of the invocation's log, and the same records are delivered to the function's log group /aws/lambda/<FunctionName>; LoggingConfig.LogFormat selects "between plain text and structured JSON". For the nodejs managed runtimes with LogFormat=Text, the runtime's documented text format (https://docs.aws.amazon.com/lambda/latest/dg/nodejs-logging.html) frames each console.* call as ONE CloudWatch Logs event of four tab-separated fields, '<ISO-8601 timestamp with milliseconds>\t<request id>\t<LEVEL>\t<message>\n', where LEVEL is the word for the console method used (INFO for console.log). The fixed text is the three tab separators and the level token; the variable parts are the timestamp, the invocation's request id and the message the handler logged. Newlines embedded INSIDE the logged string are part of that single event's message and are preserved there — they do not start a new event — and the event message ends with a newline. The contrast case is raw process.stdout.write('a\nb\n'), which is not framed at all and arrives as one event per line.

Emulator: the framing is present (the delivered event is '<ISO-8601 timestamp>\t<request id>\tINFO\t...' — four tab-separated fields with a well-formed timestamp), but the fourth field holds only the text up to the first embedded newline and carries no trailing newline. For a handler doing console.log('ML1-'+m+'\nML2-'+m+'\nML3-'+m), the single event found for that call is '<timestamp>\t<request id>\tINFO\tML1-<m>' where AWS's fourth field is 'ML1-<m>\nML2-<m>\nML3-<m>\n'. The ML2/ML3 continuation lines are not inside the event; whether they are dropped entirely or emitted as separate unframed events is not determined by this failure, because the check fails on the framed event's content before it probes for a leaked continuation line.

Why: the content of a log line the function really produced is silently lost. A caller that reads its own multi-line output back (a stack trace, a pretty-printed JSON blob, a multi-line diagnostic) sees only its first line, so log-based assertions, metric filters and subscription-filter consumers all act on a truncated message; and because the terminating newline is also missing, anything that re-joins events into a byte-exact stream produces output that differs from what the handler wrote. A multi-line message is also no longer attributable as a single logging call.

Fix: the Lambda invoke path's log-capture/Text-framing component for the nodejs managed runtimes — the rule that one console.* call produces exactly one event must apply to the whole logged string, so the embedded newlines are carried inside the framed message and the message is newline-terminated, while raw process.stdout writes continue to split one event per line.

Related already-filed work, named so it is not re-filed as the same thing: open issue 20261107T143806Z-lambda-logs-2-net-new-causes-delivered-log.md carries this check id, but it was filed when the framing was absent entirely (its replay key is the earlier `assert len(fields) == 4 and re.fullmatch(...)` framing assertion, and its Cause 2 reads "the console.log arrives as the bare string with no framing at all"). That part now passes — the timestamp, request id and INFO fields are served — and the surviving failure is on the next assertion, the multi-line message content, which that issue's description does not state as a separate requirement. It is recorded here so the remaining truncation is not closed along with the framing.

Traces (check ids): lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event.

## Evidence
Check lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event, assertion `assert fields[1:] == [rid, "INFO", f"ML1-{m}\nML2-{m}\nML3-{m}\n"]`:

AssertionError: the event must keep all three lines with their newlines: '2026-11-14T21:37:04.047Z\t800ae8a5-a7fa-4dd8-bf85-cb5ea12a14dc\tINFO\tML1-m-242774212c8b'

Handler (nodejs22.x, LogFormat=Text default):
  exports.handler = async (event) => { const m = event.marker;
    console.log("ML1-" + m + "\nML2-" + m + "\nML3-" + m);
    process.stdout.write("RAW1-" + m + "\nRAW2-" + m + "\n");
    return { ok: true }; };

Expected (AWS): exactly one event in /aws/lambda/<fn> contains 'ML1-<marker>', and splitting it on tab (maxsplit 3) yields ['<ISO-8601 ts with ms>', '<the invoke's request id>', 'INFO', 'ML1-<marker>\nML2-<marker>\nML3-<marker>\n']; no event starts with 'ML2-<marker>'; and 'RAW1-<marker>\n' and 'RAW2-<marker>\n' each appear once as their own unframed events.
Actual (emulator): the framed event's fourth field is 'ML1-<marker>' — truncated at the first embedded newline and unterminated. The timestamp '2026-11-14T21:37:04.047Z', the request id '800ae8a5-a7fa-4dd8-bf85-cb5ea12a14dc' and the marker 'm-242774212c8b' are this run's values filling the template; the requirement is the four-field shape with the complete, newline-preserving, newline-terminated message.

## Steps to Reproduce
1. Create an IAM role trusted by lambda.amazonaws.com with AWSLambdaBasicExecutionRole attached.
2. lambda.create_function(FunctionName=F, Runtime='nodejs22.x', Handler='index.handler', Role=<role arn>, MemorySize=128, Timeout=30, Code={'ZipFile': <zip of index.js>}) where index.js is:
   exports.handler = async (event) => { const m = event.marker; console.log("ML1-" + m + "\nML2-" + m + "\nML3-" + m); process.stdout.write("RAW1-" + m + "\nRAW2-" + m + "\n"); return { ok: true }; };
3. Wait for State=Active, then lambda.invoke(FunctionName=F, Payload=json.dumps({'marker': M}), LogType='Tail') and note ResponseMetadata.RequestId.
4. Poll logs.filter_log_events(logGroupName='/aws/lambda/'+F) until the 'REPORT RequestId: <rid>\t' event is present.
5. Take the single event whose message contains 'ML1-'+M and split it on tab with maxsplit 3: the fourth field is 'ML1-<M>' instead of 'ML1-<M>\nML2-<M>\nML3-<M>\n'.
6. Confirm no event starts with 'ML2-'+M (the continuation lines are not delivered as their own events either), while 'RAW1-'+M+'\n' and 'RAW2-'+M+'\n' do arrive as one event per line.
