// SPDX-License-Identifier: CC0-1.0

//! The DURABLE-EXECUTION programming model a Python handler is written against: the
//! `aws_durable_execution_sdk_python` module this runtime supplies, in Python.
//!
//! AWS Lambda's managed `python3.x` runtimes ship the durable-execution SDK on a function's
//! import path, the way they ship the AWS SDK ([`super::aws_sdk_guest`]), so a handler
//! written against the durable programming model — decorated `@durable_execution`, waiting
//! with `context.wait(Duration.from_seconds(…))`, running a unit of work through
//! `context.step(…)` — imports it without shipping a dependency of its own. This module is
//! that import path's contents here.
//!
//! What it does:
//!
//! * `@durable_execution` is what makes a handler a durable function's entry point: Lambda
//!   calls `handler(event, context)`, and the decorator hands the handler the DURABLE
//!   context — the invocation's own context plus the durable operations below — so the
//!   handler's `context.wait(…)` and `context.step(…)` resolve. Every other member a
//!   handler reads off its context (`aws_request_id`, `get_remaining_time_in_millis()`, the
//!   function's name and version) is the invocation's own, read straight through.
//! * `context.wait(duration)` waits for the span the handler asked for, so the execution is
//!   still `RUNNING` for that long and the handler continues afterwards. The duration is the
//!   handler's own parameter — nothing here shortens, lengthens or skips it — and like every
//!   other way a handler spends time it is spent inside the invocation: the wait is the
//!   standard library's own sleep, which this runtime hands the guest already clamped to what
//!   the run has left ([`super::python`]), so a wait the function's own `Timeout` cuts off
//!   ends the run as any other unfinished handler's does. A wait is CHECKPOINTED like every
//!   other operation of the model: the wait that BEGAN before the span is waited and the one
//!   that ELAPSED after it, and the one that was CANCELLED for a wait something cut off, each
//!   through the one host function that records a checkpoint
//!   ([`super::super::durable_steps`]), so the execution's history carries the wait where the
//!   handler took it.
//! * `context.step(work, …)` RUNS one STEP of the execution and answers what its work
//!   returned. `@durable_step` marks a callable as one such unit, and CALLING the marked
//!   callable NAMES that step rather than running it — `context.step(charge(order))` is how a
//!   handler runs the step it just named — while a handler that hands the callable itself to
//!   `context.step(charge, order)` names the same step inline. The work is called with
//!   the durable CONTEXT first and then the arguments the step was given, so a step can run
//!   operations of the model itself. A step is driven by its
//!   RETRY STRATEGY: the work is attempted again for as long as the strategy the step was
//!   configured with allows (`StepConfig`, `RetryStrategyConfig`), and once those attempts are
//!   spent the execution FAILS with `CallableRuntimeError` carrying the message the work's own
//!   exception carried. Every attempt is CHECKPOINTED on the execution the run belongs to —
//!   the step that was scheduled, each attempt that returned and each attempt that raised,
//!   with the attempt number the model's `RetryDetails$CurrentAttempt` reports — through the
//!   one host function that records one ([`super::super::durable_steps`]), which is how the
//!   execution's history comes to carry what the handler really did. A step that has already
//!   SUCCEEDED is never run a second time: a checkpointed step is complete, which is the
//!   at-most-once guarantee the programming model gives a unit of work.
//! * `context.invoke(function, payload)` invokes ANOTHER function as one CHAINED INVOCATION of
//!   this execution and answers what that function returned. The invocation is the same
//!   `Invoke` call a client makes, dispatched through the one seam a run reaches another service
//!   by ([`super::aws_api`]), so the invoked function RUNS, is handed the caller's payload,
//!   writes its own log and answers its own result — and an invocation the service refuses is
//!   refused exactly as a client's is. Like every other operation of the model it is
//!   CHECKPOINTED: the chained invocation that began, carrying the function it invokes and the
//!   input it carries, and then the one that answered or the one that failed, through the one
//!   host function that records a checkpoint ([`super::super::durable_steps`]), so the
//!   execution's history carries the chain the handler really took. A chained invocation that
//!   did not answer raises `CallableRuntimeError` carrying the error the service or the invoked
//!   function reported, so an uncaught one fails the execution with that error.
//! * `Duration` is the span type the model's own operations take, with the constructors a
//!   handler writes (`from_millis`, `from_seconds`, `from_minutes`, `from_hours`,
//!   `from_days`) and the readings it gives back.
//! * `context.wait_for_callback(…)` waits for something OUTSIDE the execution to finish: the
//!   durable context MINTS a callback id, hands it to the callable the handler gave so the
//!   handler can send it wherever the external work is, and then waits for a client to signal
//!   that id through `SendDurableExecutionCallbackSuccess`, `…Failure` or `…Heartbeat`. The
//!   callback and the window it is open for are the HOST's
//!   ([`super::super::durable_callbacks`]), which is where the `CallbackStarted` history event
//!   carrying the id is appended; `WaitForCallbackConfig` is the configuration a handler names
//!   the timeouts in.

use super::package::PackageModule;

/// The modules this runtime puts on a Python handler's import path as the durable-execution
/// SDK it ships.
///
/// Resolved like the AWS SDK's ([`super::aws_sdk_guest::runtime_modules`]): after the
/// deployment package's own entries, so a package shipping its own copy wins.
#[must_use]
pub fn runtime_modules() -> Vec<PackageModule> {
    [
        (
            "aws_durable_execution_sdk_python/__init__.py",
            DURABLE_SDK_SOURCE,
        ),
        (
            "aws_durable_execution_sdk_python/config.py",
            DURABLE_SDK_CONFIG_SOURCE,
        ),
        (
            "aws_durable_execution_sdk_python/retries.py",
            DURABLE_SDK_RETRIES_SOURCE,
        ),
    ]
    .into_iter()
    .map(|(path, source)| PackageModule {
        path: path.to_string(),
        source: source.to_string(),
    })
    .collect()
}

/// `aws_durable_execution_sdk_python.config`: the CONFIGURATION a handler builds for the
/// model's operations — the span type their durations are named in, a step's own
/// configuration and the retry strategy it is driven by. The types themselves are the
/// package's own ([`DURABLE_SDK_SOURCE`]), so a handler that imports one from either place
/// imports the same class.
const DURABLE_SDK_CONFIG_SOURCE: &str =
    r#""""The configuration of the durable-execution programming model's operations."""

from aws_durable_execution_sdk_python import (
    Duration,
    RetryStrategyConfig,
    StepConfig,
    WaitForCallbackConfig,
)

__all__ = [
    "Duration",
    "RetryStrategyConfig",
    "StepConfig",
    "WaitForCallbackConfig",
]
"#;

/// `aws_durable_execution_sdk_python.retries`: the RETRY STRATEGY a step is driven by, which
/// is the one the package itself defines ([`DURABLE_SDK_SOURCE`]) — so a handler that
/// imports it from either place imports the same class.
const DURABLE_SDK_RETRIES_SOURCE: &str =
    r#""""The retry strategies a step of a durable execution is driven by."""

from aws_durable_execution_sdk_python import (
    Duration,
    RetryStrategyConfig,
    create_retry_strategy,
)

__all__ = [
    "Duration",
    "RetryStrategyConfig",
    "create_retry_strategy",
]
"#;

/// `aws_durable_execution_sdk_python`: the names a durable handler imports.
const DURABLE_SDK_SOURCE: &str = r#""""The durable-execution programming model for Python handlers."""

import base64 as _base64
import json as _json
import time

_host = __import__("_lambda_host")

__all__ = [
    "CallableRuntimeError",
    "CallbackFailureError",
    "CallbackTimeoutError",
    "Duration",
    "DurableContext",
    "RetryStrategyConfig",
    "StepConfig",
    "WaitForCallbackConfig",
    "create_retry_strategy",
    "durable_execution",
    "durable_step",
]


class CallableRuntimeError(Exception):
    """What an operation of this model whose work did not answer fails the execution with.

    A STEP is attempted again for as long as its retry strategy allows; once those attempts
    are spent, the execution fails with this error carrying the message the work's own
    exception carried. A CHAINED INVOCATION that did not answer -- one the service refused, one
    whose invoked function failed -- raises it carrying the message that service or that
    function reported.
    """


class CallbackTimeoutError(Exception):
    """What a WAIT FOR CALLBACK whose window closed raises.

    The callback was open for the timeout its configuration named; once that window closed
    nothing can signal it any more, so the wait raises here and the execution fails with it
    unless the handler catches it.
    """


class CallbackFailureError(Exception):
    """What a WAIT FOR CALLBACK a client FAILED raises, carrying that client's own error."""


class Duration:
    """A span of time, as this model's own operations take one."""

    def __init__(self, seconds):
        self._seconds = float(seconds)

    @classmethod
    def from_millis(cls, millis):
        return cls(float(millis) / 1000.0)

    @classmethod
    def from_seconds(cls, seconds):
        return cls(seconds)

    @classmethod
    def from_minutes(cls, minutes):
        return cls(float(minutes) * 60.0)

    @classmethod
    def from_hours(cls, hours):
        return cls(float(hours) * 3600.0)

    @classmethod
    def from_days(cls, days):
        return cls(float(days) * 86400.0)

    def total_seconds(self):
        return self._seconds

    def total_millis(self):
        return int(self._seconds * 1000.0)

    def __eq__(self, other):
        if isinstance(other, Duration):
            return self._seconds == other._seconds
        return NotImplemented

    def __hash__(self):
        return hash(self._seconds)

    def __repr__(self):
        return "Duration(seconds=%r)" % (self._seconds,)


class RetryStrategyConfig:
    """How many attempts a step has, and how long this model waits between two of them.

    `max_attempts` counts every attempt of the step, the first one included, so a strategy
    of one attempt is a step that is not retried. The interval is the span before the
    attempt that follows a failed one, grown by `backoff_rate` per attempt already made and
    never past `max_interval`; a strategy that names no interval is attempted again at once.
    """

    def __init__(
        self,
        max_attempts=1,
        initial_interval=None,
        backoff_rate=1.0,
        max_interval=None,
        **options
    ):
        self.max_attempts = max_attempts
        self.initial_interval = initial_interval
        self.backoff_rate = backoff_rate
        self.max_interval = max_interval
        self.options = options

    def __repr__(self):
        return "RetryStrategyConfig(max_attempts=%r, initial_interval=%r)" % (
            self.max_attempts,
            self.initial_interval,
        )


class StepConfig:
    """The configuration one step runs under: its name, and its retry strategy."""

    def __init__(self, retry_strategy=None, name=None, **options):
        self.retry_strategy = retry_strategy
        self.name = name
        self.options = options

    def __repr__(self):
        return "StepConfig(name=%r, retry_strategy=%r)" % (self.name, self.retry_strategy)


class WaitForCallbackConfig:
    """The configuration one WAIT FOR CALLBACK runs under.

    `timeout` is how long the callback stays open: once it passes, nothing can signal the
    callback any more and the wait raises `CallbackTimeoutError`. `heartbeat_timeout` is how
    long the wait goes without a heartbeat before the callback closes the same way; a
    configuration that names none asks for no heartbeats at all.
    """

    def __init__(self, timeout=None, heartbeat_timeout=None, name=None, **options):
        self.timeout = timeout
        self.heartbeat_timeout = heartbeat_timeout
        self.name = name
        self.options = options

    def __repr__(self):
        return "WaitForCallbackConfig(timeout=%r, heartbeat_timeout=%r)" % (
            self.timeout,
            self.heartbeat_timeout,
        )


def _seconds_of(duration):
    """The span `duration` names, in seconds."""
    if isinstance(duration, Duration):
        return duration.total_seconds()
    if isinstance(duration, bool):
        raise TypeError("a duration is a Duration or a number of seconds, not a bool")
    if isinstance(duration, (int, float)):
        return float(duration)
    reading = getattr(duration, "total_seconds", None)
    if reading is not None:
        return float(reading())
    millis = getattr(duration, "total_millis", None)
    if millis is not None:
        return float(millis()) / 1000.0
    raise TypeError(
        "a duration is a Duration or a number of seconds, not %r" % (type(duration),)
    )


def _member(holder, *names):
    """The first of `names` that `holder` carries, as a mapping key or as an attribute."""
    if holder is None:
        return None
    for name in names:
        if isinstance(holder, dict):
            if name in holder:
                return holder[name]
            continue
        value = getattr(holder, name, None)
        if value is not None:
            return value
    return None


def create_retry_strategy(config=None, **options):
    """The retry strategy a step is driven by, as a handler names one.

    Named with the configuration itself (`create_retry_strategy(RetryStrategyConfig(...))`),
    with the members of one (`create_retry_strategy(max_attempts=2)`), or with both — a
    member named beside a configuration is the one in force. The answer is the strategy a
    step's own configuration carries (`StepConfig.retry_strategy`).
    """
    named = {}
    if isinstance(config, dict):
        named.update(config)
    elif config is not None:
        for member in ("max_attempts", "initial_interval", "backoff_rate", "max_interval"):
            value = _member(config, member)
            if value is not None:
                named[member] = value
    named.update(options)
    return RetryStrategyConfig(**named)


def _retry_strategy_of(config):
    """The retry strategy a step configured with `config` runs under.

    The strategy is the configuration's own (`StepConfig.retry_strategy`), and a caller that
    passed the strategy AS the step's configuration is read as having named one.
    """
    if config is None:
        return None
    strategy = _member(config, "retry_strategy", "retryStrategy")
    if strategy is not None:
        return strategy
    if _member(config, "max_attempts", "maxAttempts") is not None:
        return config
    return None


def _max_attempts_of(strategy):
    """How many attempts the step has in all: every attempt, the first one included."""
    try:
        attempts = int(_member(strategy, "max_attempts", "maxAttempts"))
    except (TypeError, ValueError):
        return 1
    return attempts if attempts >= 1 else 1


def _interval_after(strategy, attempt):
    """How long before the attempt that follows the one numbered `attempt`, in seconds."""
    initial = _member(strategy, "initial_interval", "initialInterval", "interval", "delay")
    if initial is None:
        return 0.0
    try:
        seconds = _seconds_of(initial)
    except TypeError:
        return 0.0
    try:
        rate = float(_member(strategy, "backoff_rate", "backoffRate") or 1.0)
    except (TypeError, ValueError):
        rate = 1.0
    if rate > 0.0 and attempt > 1:
        seconds = seconds * (rate ** (attempt - 1))
    ceiling = _member(strategy, "max_interval", "maxInterval")
    if ceiling is not None:
        try:
            seconds = min(seconds, _seconds_of(ceiling))
        except TypeError:
            pass
    return seconds if seconds > 0.0 else 0.0


def _result_payload(value):
    """The JSON a step's result is checkpointed as, and no payload for one that is not JSON."""
    try:
        return _json.dumps(value)
    except BaseException:
        return None


# What ONE chained invocation is: the same `Invoke` call a client makes, made through the one
# seam a run reaches another service by, and made SYNCHRONOUSLY because the answer is what the
# handler that took the invocation reads back.
_INVOKE_SERVICE = "lambda"
_INVOKE_OPERATION = "Invoke"
_INVOKE_TYPE = "RequestResponse"

# The `OperationType` of a chained invocation and the `OperationAction`s taken on it, as the
# model names them (`OperationUpdate$Type`, `OperationUpdate$Action`).
_CHAINED_INVOKE = "CHAINED_INVOKE"


def _first_named(options, *names):
    """The first of `names` that `options` carries a value under."""
    for name in names:
        held = options.get(name)
        if held is not None:
            return held
    return None


def _invoke_payload_text(payload):
    """The event a chained invocation carries, as the JSON text `Invoke$Payload` takes.

    Text and bytes are the document the caller rendered itself, which is the form that member
    carries one in; any other value IS the document, so it is rendered here.
    """
    if payload is None:
        return None
    if isinstance(payload, (bytes, bytearray)):
        return bytes(payload).decode("utf-8", "replace")
    if isinstance(payload, str):
        return payload
    return _json.dumps(payload)


def _invoke_call(target, input_text, tenant_id):
    """Invoke `target` and answer what the service said.

    The invocation is dispatched through the host's own call seam, so it is the very `Invoke`
    request a client makes: the invoked function RUNS, writes its own log and answers its own
    payload, and an invocation the service refuses is refused exactly as a client's is.

    `tenant_id` is the RUN's own tenant, never a value the handler named, so a chained
    invocation addresses the tenant of the execution that took it ([`DurableContext.invoke`]).

    The REGION is the invocation's own: the seam replaces a value that is no Region name with
    it, so a chained invocation names none and reaches the endpoint this run was addressed at.
    """
    request = {"FunctionName": target, "InvocationType": _INVOKE_TYPE}
    if input_text is not None:
        request["Payload"] = _base64.b64encode(input_text.encode("utf-8")).decode("ascii")
    if tenant_id is not None:
        request["TenantId"] = str(tenant_id)
    answered = _host.aws(_INVOKE_SERVICE, _INVOKE_OPERATION, "", _json.dumps(request))
    try:
        answered = _json.loads(answered)
    except ValueError:
        return {"refused": "not_made", "detail": "the chained invocation was not answered"}
    return answered if isinstance(answered, dict) else {}


def _answered_document(answered):
    """The output document a call answered, keyed by the operation's own output members."""
    text = answered.get("body") or ""
    try:
        document = _json.loads(text) if text else {}
    except ValueError:
        return {}
    return document if isinstance(document, dict) else {}


def _answered_text(answered, document, member):
    """The text one member of the answer carries, decoded from the base64 a blob travelled as.

    WHICH of the answer's members carry bytes is the called operation's own model's to say, and
    the host names them beside the answer, so a member it named is decoded and any other is
    read as the text it already is.
    """
    held = document.get(member)
    if not isinstance(held, str):
        return ""
    carried = list(answered.get("payloadBlobs") or []) + list(answered.get("streams") or [])
    if member not in carried:
        return held
    try:
        return _base64.b64decode(held).decode("utf-8", "replace")
    except (TypeError, ValueError):
        return ""


def _refused_sentence(document):
    """The message a refusal carried, under either spelling this wire uses for it."""
    for member in ("message", "Message", "errorMessage"):
        held = document.get(member)
        if isinstance(held, str):
            return held
    return ""


# The separator a shape id carries between its namespace and the shape's own name, which is
# what an error class arrives named by.
_SHAPE_ID_SEPARATOR = chr(35)


def _refused_class(answered, document):
    """The error class a refusal named, as the service answered it, and none for one that
    named none."""
    for named in (answered.get("errorType"), document.get("__type"), document.get("Code")):
        if not isinstance(named, str):
            continue
        shape = named.rpartition(_SHAPE_ID_SEPARATOR)[2].split(":")[0].strip()
        if shape:
            return shape
    return None


def _error_document(text):
    """The error document a FAILED invocation answered, as a runtime writes one."""
    try:
        document = _json.loads(text)
    except ValueError:
        return {}
    return document if isinstance(document, dict) else {}


def _invoked_result(text):
    """What the invoked function returned, as the handler reads it: the JSON it answered, the
    text itself for an answer that is not JSON, and nothing for one that answered nothing."""
    if not text:
        return None
    try:
        return _json.loads(text)
    except ValueError:
        return text


# How long this model waits between two readings of a callback it is waiting on, in seconds.
#
# The window a callback is open for is the HOST's, and so is the instant it closes, so a reading
# only ever reports what already happened: this is how often the wait looks. Short against the
# shortest timeout a handler would configure, and long enough that a wait of minutes is a
# handful of readings a second rather than a spin.
_CALLBACK_POLL_SECONDS = 0.05


def _whole_seconds(duration):
    """`duration` as the whole number of seconds a timeout is named in, and 0 for none.

    `CallbackOptions$TimeoutSeconds` and `HeartbeatTimeoutSeconds` are whole numbers of
    seconds, and 0 is what each of them means by "not specified": no timeout, and heartbeats
    disabled.
    """
    if duration is None:
        return 0
    try:
        seconds = _seconds_of(duration)
    except TypeError:
        return 0
    if seconds <= 0:
        return 0
    return int(seconds + 0.5)


def _callback_call(asked):
    """One call about a CALLBACK of this execution, with what the host refused raised.

    The REFUSAL of the call itself travels under its own member: a callback a client FAILED
    carries an error of its own, which is the waiting handler's answer rather than a refusal.
    """
    answered = _json.loads(_host.durable_callback(_json.dumps(asked)))
    refused = answered.get("refused")
    if refused:
        raise CallableRuntimeError(refused)
    return answered


def _hand_callback_to(work, context, callback_id):
    """Hand `callback_id` to the handler's own callable, which is what carries it outside.

    The callable is called with the arguments it takes: the id alone, the context and the id,
    or neither — so a handler writes the callable the way it reads best and the id reaches it
    either way.
    """
    if work is None:
        return None
    if not callable(work):
        raise TypeError(
            "a callback's work is a callable that is handed the callback id, not %r"
            % (type(work),)
        )
    target = getattr(work, "__wrapped__", work)
    code = getattr(target, "__code__", None)
    if code is None:
        call = getattr(target, "__call__", None)
        code = getattr(call, "__code__", None)
        if code is not None and hasattr(call, "__self__"):
            # A bound call carries its own object, which is not one of the arguments the
            # caller supplies.
            return _called_with(work, max(code.co_argcount - 1, 0), context, callback_id)
    if code is None:
        return work(callback_id)
    takes = code.co_argcount
    if hasattr(target, "__self__"):
        takes = max(takes - 1, 0)
    return _called_with(work, takes, context, callback_id)


def _called_with(work, takes, context, callback_id):
    """Call `work` with as many of the context and the callback id as it takes."""
    if takes == 0:
        return work()
    if takes == 1:
        return work(callback_id)
    return work(context, callback_id)


def _named_like(wrapper, target):
    """Carry `target`'s own name and documentation onto the wrapper around it."""
    for member in ("__name__", "__qualname__", "__doc__", "__module__"):
        try:
            setattr(wrapper, member, getattr(target, member))
        except AttributeError:
            pass
    wrapper.__wrapped__ = target
    return wrapper


class _DeferredStep:
    """One STEP of a durable execution: the work, the arguments it was named with, and its
    configuration.

    What a `@durable_step` callable answers when it is CALLED — naming a step is not running
    it. `DurableContext.step` is what runs it, under the retry strategy its configuration
    carries, so a step a handler named and never handed to the context never runs at all.

    A named step is not the step's RESULT either, and it refuses to stand in for one: using it
    as a value — as a truth, a sequence, a mapping or an attribute of its own — raises here,
    so a handler that forgot to run it fails loudly instead of carrying an answer its step
    never produced.
    """

    def __init__(self, work, args, kwargs, name=None, config=None):
        self.work = work
        self.args = args
        self.kwargs = kwargs
        self.name = name or getattr(work, "__name__", "step")
        self.config = config

    def __repr__(self):
        return "DurableStep(name=%r)" % (self.name,)

    def _unrun(self):
        return TypeError(
            "the step %r has been named but not run: hand it to context.step(...) to run it "
            "and answer its result" % (self.name,)
        )

    def __getattr__(self, name):
        raise AttributeError(
            "the step %r has been named but not run, so it carries no %r: hand it to "
            "context.step(...) to run it" % (self.__dict__.get("name"), name)
        )

    def __bool__(self):
        raise self._unrun()

    def __len__(self):
        raise self._unrun()

    def __iter__(self):
        raise self._unrun()

    def __getitem__(self, key):
        raise self._unrun()


class DurableContext:
    """One invocation's context, with this model's durable operations on it.

    Every member this class does not define is the invocation context's own.
    """

    def __init__(self, context):
        self._context = context
        self._steps = 0
        self._waits = 0
        self._callbacks = 0
        self._invokes = 0

    def __getattr__(self, name):
        return getattr(self._context, name)

    def __repr__(self):
        return "DurableContext(%r)" % (self._context,)

    @property
    def context(self):
        """The invocation context this durable context was built from."""
        return self._context

    def wait(self, duration, name=None, **options):
        """Wait for `duration` before the handler continues.

        The wait is one OPERATION of this execution, so it is checkpointed as one: the wait
        that began, then the wait that elapsed — or the wait that was CANCELLED, for one
        something cut off before its span was up, carrying the error that cut it off.
        """
        seconds = _seconds_of(duration)
        if seconds < 0:
            seconds = 0.0
        self._waits += 1
        identifier = "wait-%d" % self._waits
        self._checkpoint(
            "START", identifier, name, 1, operation="WAIT", wait_seconds=seconds
        )
        try:
            if seconds > 0:
                time.sleep(seconds)
        except BaseException as error:
            self._checkpoint(
                "CANCEL",
                identifier,
                name,
                1,
                operation="WAIT",
                wait_seconds=seconds,
                error={
                    "errorType": type(error).__name__,
                    "errorMessage": str(error),
                },
            )
            raise
        self._checkpoint(
            "SUCCEED", identifier, name, 1, operation="WAIT", wait_seconds=seconds
        )

    def wait_for_callback(self, work=None, config=None, name=None, timeout=None, **options):
        """Wait for something OUTSIDE this execution to finish, and answer what it sent.

        The durable context MINTS a callback id and hands it to `work` — the callable the
        handler gave, which is how the handler sends that id wherever the external work is —
        and then waits for a client to signal it: `SendDurableExecutionCallbackSuccess` is what
        this call answers, `SendDurableExecutionCallbackFailure` is what it raises, and
        `SendDurableExecutionCallbackHeartbeat` keeps the window open while the external work
        is still running.

        The window is the `timeout` the configuration named: once it closes, nothing can signal
        the callback any more and this raises `CallbackTimeoutError`. Like every other way a
        handler spends time, the wait is spent INSIDE the invocation, so a wait the function's
        own `Timeout` cuts off ends the run as any other unfinished handler's does.
        """
        held = config if config is not None else options.get("callback_config")
        if timeout is None:
            timeout = _member(held, "timeout", "timeout_seconds", "timeoutSeconds")
        heartbeat = _member(
            held, "heartbeat_timeout", "heartbeat_timeout_seconds", "heartbeatTimeoutSeconds"
        )
        if name is None:
            name = _member(held, "name")
        self._callbacks += 1
        identifier = "callback-%d" % self._callbacks
        asked = {
            "action": "START",
            "id": identifier,
            "timeoutSeconds": _whole_seconds(timeout),
            "heartbeatTimeoutSeconds": _whole_seconds(heartbeat),
        }
        if name is not None:
            asked["name"] = str(name)
        started = _callback_call(asked)
        callback_id = started["callbackId"]
        # The handler's own callable is what carries the id to the external system, so it is
        # called with the id the context minted — and with this context too, for a callable
        # that takes one.
        _hand_callback_to(work, self, callback_id)
        while True:
            answered = _callback_call({"action": "POLL", "callbackId": callback_id})
            state = answered.get("state")
            if state == "SUCCEEDED":
                result = answered.get("result")
                if result is None:
                    return None
                try:
                    return _json.loads(result)
                except ValueError:
                    # A client may send a result that is not JSON, and what the waiting
                    # handler receives is then the text itself.
                    return result
            if state == "FAILED":
                error = answered.get("error") or {}
                raise CallbackFailureError(
                    error.get("errorMessage") or error.get("errorType") or ""
                )
            if state == "TIMED_OUT":
                error = answered.get("error") or {}
                raise CallbackTimeoutError(error.get("errorMessage") or "")
            # Still waiting: the window is the host's, and so is the instant it closes. The
            # wait between two readings is the standard library's own sleep, which this runtime
            # clamps to what is left of the invocation.
            time.sleep(_CALLBACK_POLL_SECONDS)

    def invoke(self, function_name=None, payload=None, config=None, name=None, **options):
        """Invoke ANOTHER function as one CHAINED INVOCATION of this execution, and answer
        what it returned.

        `function_name` is the name or ARN of the function to invoke
        (`ChainedInvokeOptions$FunctionName`) and `payload` is the event it is handed; either
        may be named on a configuration passed beside them instead. The invocation is one
        OPERATION of this execution, so it is CHECKPOINTED like every other one: the chained
        invocation that STARTED, carrying the function it invokes and the input it carries, and
        then the one that ANSWERED, carrying the answer -- or the one that FAILED, carrying the
        error it failed with.

        What comes back is what the invoked function returned. An invocation the service
        REFUSED -- a function that does not exist among them -- and one whose function FAILED
        both raise `CallableRuntimeError` carrying that error's own message, so a handler that
        does not catch it fails this execution with the error the chain failed on.

        The TENANT the invocation is made under is the RUN's own, read off the invocation
        context this durable context was built from: a chained invocation stays inside the
        tenant of the execution that took it, so no call of this operation can name another
        one.
        """
        target = function_name
        if target is None:
            target = _first_named(
                options, "function_name", "functionName", "FunctionName", "target"
            )
        if target is None:
            target = _member(config, "function_name", "functionName", "FunctionName")
        if not isinstance(target, str) or not target:
            raise TypeError(
                "a chained invocation names the function to invoke, not %r" % (target,)
            )
        held = payload
        if held is None:
            held = _first_named(options, "payload", "Payload", "input", "event")
        if held is None:
            held = _member(config, "payload", "input")
        tenant = getattr(self._context, "tenant_id", None)
        if name is None:
            name = _member(config, "name")
        input_text = _invoke_payload_text(held)
        self._invokes += 1
        identifier = "invoke-%d" % self._invokes
        self._checkpoint(
            "START",
            identifier,
            name,
            1,
            operation=_CHAINED_INVOKE,
            payload=input_text,
            function_name=target,
            tenant_id=tenant,
        )
        answered = _invoke_call(target, input_text, tenant)
        refused = answered.get("refused")
        if refused:
            # The call was not MADE at all, so nothing invoked the function: the operation
            # failed for the reason the host gives rather than for anything a service said.
            raise self._chain_failed(
                identifier, name, target, answered.get("detail") or refused, None
            )
        status = answered.get("status") or 0
        document = _answered_document(answered)
        if not 200 <= status < 300:
            # The SERVICE refused the invocation, and its own refusal is what this operation
            # failed with: a function that does not exist fails the execution with the
            # service's own function-not-found sentence.
            raise self._chain_failed(
                identifier,
                name,
                target,
                _refused_sentence(document),
                _refused_class(answered, document),
            )
        carried = _answered_text(answered, document, "Payload")
        if document.get("FunctionError"):
            # The function RAN and failed. What such an invocation answers is that run's own
            # error document, which is the error this operation failed with.
            failed = _error_document(carried)
            raise self._chain_failed(
                identifier,
                name,
                target,
                failed.get("errorMessage") or carried,
                failed.get("errorType") or document.get("FunctionError"),
            )
        self._checkpoint(
            "SUCCEED",
            identifier,
            name,
            1,
            operation=_CHAINED_INVOKE,
            payload=carried or None,
            function_name=target,
            tenant_id=tenant,
        )
        return _invoked_result(carried)

    def _chain_failed(self, identifier, name, target, message, code):
        """Checkpoint the chained invocation `identifier` as FAILED, and answer the error to
        raise for it.

        The CHECKPOINT carries the error class the service or the invoked function named, which
        is what the operation's own history event reports; what the handler is RAISED is the
        model's own error for an operation whose work did not answer.
        """
        self._checkpoint(
            "FAIL",
            identifier,
            name,
            1,
            operation=_CHAINED_INVOKE,
            function_name=target,
            error={
                "errorType": code or "CallableRuntimeError",
                "errorMessage": message,
            },
        )
        return CallableRuntimeError(message)

    def step(self, work, *args, **kwargs):
        """Run one STEP of this execution and answer what its work returned.

        `work` is the step a `@durable_step` callable NAMED when the handler called it
        (`context.step(charge(order))`), or the callable itself with the arguments to run it
        with (`context.step(charge, order)`). Either way the work is called with the durable
        CONTEXT first and then those arguments, so a step can run operations of the model
        itself, and it is driven by the retry strategy its configuration carries — the one
        this call names, or the one the step was named with.
        """
        name = kwargs.pop("name", None)
        config = kwargs.pop("config", None)
        held = kwargs.pop("step_config", None)
        options = kwargs.pop("options", None)
        if config is None:
            config = held if held is not None else options
        if isinstance(work, _DeferredStep):
            # A step the handler NAMED: its work, its arguments and its configuration are
            # that naming's own, and what this call names wins over them. A CONFIGURATION may
            # be named beside the step itself; anything else beside it is not an argument of a
            # step that already carries its own, and is refused rather than read as one.
            for named in args:
                if _retry_strategy_of(named) is None:
                    raise TypeError(
                        "beside a step that is already named, this call takes its "
                        "configuration and nothing else, not %r" % (type(named),)
                    )
                if config is None:
                    config = named
            return self._stepped(
                work.work,
                name or work.name,
                config if config is not None else work.config,
                work.args,
                work.kwargs,
            )
        return self._stepped(work, name, config, args, kwargs)

    def _stepped(self, work, name, config, args, kwargs):
        """Drive one step of this execution: its attempts, and the checkpoint of each."""
        target = getattr(work, "__wrapped__", work)
        if not callable(target):
            raise TypeError(
                "a step's work is a callable or a step named by one, not %r"
                % (type(work),)
            )
        if config is None:
            config = getattr(work, "durable_step_config", None)
        if name is None:
            name = getattr(work, "durable_step_name", None) or getattr(
                target, "__name__", "step"
            )
        strategy = _retry_strategy_of(config)
        attempts = _max_attempts_of(strategy)
        self._steps += 1
        identifier = "step-%d" % self._steps
        self._checkpoint("START", identifier, name, 1)
        attempt = 1
        # Every attempt, and every interval between two of them, is spent INSIDE the
        # invocation: the run's own deadline and step budget are what bound them, exactly as
        # they bound any other work the handler does.
        while True:
            try:
                result = target(self, *args, **kwargs)
            except Exception as error:
                spent = attempt >= attempts
                delay = None if spent else _interval_after(strategy, attempt)
                self._checkpoint(
                    "FAIL",
                    identifier,
                    name,
                    attempt,
                    error={
                        "errorType": type(error).__name__,
                        "errorMessage": str(error),
                    },
                    next_attempt_delay=delay,
                )
                if spent:
                    raise CallableRuntimeError(str(error))
                if delay > 0:
                    time.sleep(delay)
                attempt += 1
                continue
            self._checkpoint(
                "SUCCEED",
                identifier,
                name,
                attempt,
                payload=_result_payload(result),
            )
            return result

    def _checkpoint(
        self,
        action,
        identifier,
        name,
        attempt,
        payload=None,
        error=None,
        next_attempt_delay=None,
        operation="STEP",
        wait_seconds=None,
        function_name=None,
        tenant_id=None,
    ):
        """Record one thing that happened to an operation of the execution this run belongs
        to."""
        record = {
            "type": operation,
            "action": action,
            "id": identifier,
            "attempt": attempt,
        }
        if wait_seconds is not None:
            # `WaitOptions$WaitSeconds` and `WaitStartedDetails$Duration` are whole numbers
            # of seconds, so a span named finer than that is reported as the nearest second.
            record["waitSeconds"] = int(wait_seconds + 0.5)
        if function_name is not None:
            # `ChainedInvokeOptions$FunctionName`: the function a chained invocation invokes,
            # as the operation NAMED it.
            record["functionName"] = str(function_name)
        if tenant_id is not None:
            record["tenantId"] = str(tenant_id)
        if name is not None:
            record["name"] = str(name)
        if payload is not None:
            record["payload"] = payload
        if error is not None:
            record["error"] = error
        if next_attempt_delay is not None:
            # `RetryDetails$NextAttemptDelaySeconds` is a whole number of seconds, so a span
            # named finer than that is reported as the nearest second rather than cut to zero.
            record["nextAttemptDelaySeconds"] = int(next_attempt_delay + 0.5)
        _host.durable(_json.dumps(record))


def durable_execution(handler=None, **options):
    """Mark a handler as a durable function's entry point.

    Usable bare (`@durable_execution`) and called (`@durable_execution(...)`).
    """

    def decorate(target):
        def invoke(event, context):
            return target(event, DurableContext(context))

        invoke.durable_execution = True
        return _named_like(invoke, target)

    if handler is None:
        return decorate
    return decorate(handler)


def durable_step(work=None, name=None, config=None, retry_strategy=None, **options):
    """Mark a callable as one unit of work of a durable execution.

    Usable bare (`@durable_step`) and called (`@durable_step(name="...",
    config=StepConfig(...))`, or with the configuration itself). CALLING the marked callable
    NAMES one step of the execution and answers it rather than running it
    (`context.step(charge(order))`): the step is run by `DurableContext.step`, which is also
    where the configuration this decorator carries is read.
    """

    if config is None and retry_strategy is not None:
        config = StepConfig(retry_strategy=retry_strategy)
    # A decorator CALLED with the step's own configuration rather than with the callable:
    # `@durable_step(StepConfig(...))` names the configuration, `@durable_step("name")` the
    # step's name, and the callable is what the returned decorator is then applied to.
    if work is not None and not callable(work):
        if isinstance(work, str):
            name = name if name is not None else work
        elif config is None:
            config = work
        work = None

    def decorate(target):
        def name_step(*args, **kwargs):
            return _DeferredStep(target, args, kwargs, name=name, config=config)

        name_step.durable_step = True
        name_step.durable_step_name = name or getattr(target, "__name__", "step")
        name_step.durable_step_config = config
        return _named_like(name_step, target)

    if work is None:
        return decorate
    return decorate(work)
"#;
