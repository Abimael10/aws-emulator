Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event
Issue-Key: lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event|run#17|AssertionError|assert msgs.count(f"RAW1-{m}\n") == 1 and msgs.count(f"RAW2-{m}\n") == 1, (
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# Lambda log delivery concatenates a handler's successive stdout lines into one CloudWatch Logs event instead of one event per line

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: raw stdout written by the handler is delivered to CloudWatch Logs as a single event containing several lines.

AWS: in TEXT log format, each newline-terminated line a function writes to stdout/stderr becomes its own CloudWatch Logs event; a handler that writes 'RAW1-<marker>\n' and then 'RAW2-<marker>\n' produces two events whose messages are exactly those two lines (Lambda developer guide, 'Using CloudWatch logs with Lambda' — only a multi-line record emitted as one write, such as a traceback, is kept together). Recorded behavior only: log framing is not described by the Smithy model.
Emulator: FilterLogEvents/GetLogEvents returns ONE event whose message is 'RAW1-<marker>\nRAW2-<marker>\n' — both lines concatenated into a single event.
Why: per-line events are what makes Lambda logs usable: metric filters, subscription filters, log-level filters and any consumer counting occurrences of a line all see one event where AWS delivers two, so counts and pattern matches are wrong and a per-line filter can match a line the caller never intended to expose.
Fix: the Lambda log forwarder / execution-environment log tap — split the captured stdout/stderr stream on newlines and emit one CloudWatch Logs event per line, keeping only a genuinely multi-line single write (a traceback block) as one event.
Traces: lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event.

## Evidence
Behavioral suite replay against the emulator built from this checkout; service lambda__logs 26/27.

[mismatch] lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event:
  File check_lambda__logs_log_cwl_node_text_multiline_console_log_is_one_event.py, line 68, in run
  assert msgs.count(f"RAW1-{m}\n") == 1 and msgs.count(f"RAW2-{m}\n") == 1
  AssertionError: raw stdout must arrive as one event per line: ['RAW1-m-e04259680f25\nRAW2-m-e04259680f25\n']

Expected: two log events, messages 'RAW1-<marker>\n' and 'RAW2-<marker>\n'. Actual: one event carrying both lines. (The marker is this run's generated value.)

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch.
2. Create a Node.js function (TEXT log format) whose handler writes two separate newline-terminated lines to stdout.
3. Invoke it, then read the function's log group with FilterLogEvents.
4. Observe one event containing both lines instead of one event per line; replay lambda__logs.log-cwl-node-text-multiline-console-log-is-one-event.
