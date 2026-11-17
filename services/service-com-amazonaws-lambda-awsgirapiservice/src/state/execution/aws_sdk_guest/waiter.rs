// SPDX-License-Identifier: CC0-1.0

//! The WAITERS of the AWS SDK a Python handler imports: `botocore.waiter`, the module
//! `client.get_waiter(...)` answers out of.
//!
//! The managed `python3.x` runtimes ship the whole AWS SDK for Python, so a handler that has
//! just asked for a resource and must not go on until it is usable asks the client for the
//! waiter of that state and waits on it rather than writing the poll loop itself. This module
//! is that interface: the waiter of one name, and the acceptors that decide when its wait is
//! over.
//!
//! What it does and does not do:
//!
//! * One poll is ONE call of the waiter's own operation, made through the client's own method
//!   — so the request, the signature, the destination and every bound on it are the ones every
//!   other call of that client is made under ([`super::super::aws_api`]), and what an acceptor
//!   judges is the service's own answer. Nothing here reads or writes state of its own.
//! * WHICH operation a waiter polls, what it accepts, how long it waits between polls and how
//!   many polls it makes are the service's own waiter definition's ([`WAITER_SOURCE`]'s
//!   catalog). A name the service publishes no waiter for does not exist, which is what the
//!   SDK says rather than polling something else. The member path an acceptor judges is read
//!   by the one reader of a path this SDK has, the `botocore.search_path` the paginator reads
//!   its own members with ([`super`]).
//! * A refusal is an answer a waiter judges: the acceptors of a waiter for a resource that is
//!   not there yet match on the error code, so the refusal's own response document is what a
//!   poll hands back, exactly as the SDK's own waiter does — and a refusal no acceptor names
//!   ends the wait with `WaiterError` rather than being retried forever.
//! * The wait ENDS: a success acceptor returns, a failure acceptor raises `WaiterError`
//!   carrying the state it matched, and a wait that runs out of attempts raises it too. The
//!   wait between polls is the standard library's own `sleep`, which this runtime hands the
//!   guest already clamped to what is left of the invocation ([`super::super::python`]), so no
//!   wait outlasts the run it is made in.
//! * `WaiterConfig` is honoured the way the SDK honours it: `Delay` and `MaxAttempts` passed
//!   to `wait(...)` replace the definition's own for that one wait.

/// The modules of the waiter interface, in the order they are put on the import path.
#[must_use]
pub(super) fn modules() -> Vec<(&'static str, &'static str)> {
    vec![("botocore/waiter.py", WAITER_SOURCE)]
}

/// `botocore.waiter`: the waiter of one name, its acceptors, and the services' definitions.
const WAITER_SOURCE: &str = r#"
"""Waiting for a resource to reach a state, as the SDK's own waiters do."""

import time

from botocore import search_path, xform_name
from botocore.exceptions import ClientError, WaiterError

# The waiters each service publishes, as the AWS SDKs' own waiter data (the `waiters-2.json`
# the SDKs ship for a service) states them, keyed by the service a client is built for and
# then by the waiter's own name. Each entry names the operation one poll is a call of, the
# seconds a poll waits before the next one (`delay`), how many polls the wait makes
# (`maxAttempts`) and the acceptors that decide its outcome, each with the state it moves the
# wait to, the matcher that judges an answer and what that matcher expects.
WAITERS = {
    "cloudformation": {
        "StackExists": {
            "operation": "DescribeStacks",
            "delay": 5,
            "maxAttempts": 20,
            "acceptors": [
                {"state": "success", "matcher": "status", "expected": 200},
                {"state": "retry", "matcher": "error", "expected": "ValidationError"},
            ],
        },
        "StackCreateComplete": {
            "operation": "DescribeStacks",
            "delay": 30,
            "maxAttempts": 120,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "pathAll",
                    "argument": "Stacks[].StackStatus",
                    "expected": "CREATE_COMPLETE",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "CREATE_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "DELETE_COMPLETE",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "DELETE_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "ROLLBACK_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "ROLLBACK_COMPLETE",
                },
                {"state": "failure", "matcher": "error", "expected": "ValidationError"},
            ],
        },
        "StackUpdateComplete": {
            "operation": "DescribeStacks",
            "delay": 30,
            "maxAttempts": 120,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "pathAll",
                    "argument": "Stacks[].StackStatus",
                    "expected": "UPDATE_COMPLETE",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "UPDATE_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "UPDATE_ROLLBACK_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "UPDATE_ROLLBACK_COMPLETE",
                },
                {"state": "failure", "matcher": "error", "expected": "ValidationError"},
            ],
        },
        "StackDeleteComplete": {
            "operation": "DescribeStacks",
            "delay": 30,
            "maxAttempts": 120,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "pathAll",
                    "argument": "Stacks[].StackStatus",
                    "expected": "DELETE_COMPLETE",
                },
                {"state": "success", "matcher": "error", "expected": "ValidationError"},
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "DELETE_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "CREATE_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "ROLLBACK_FAILED",
                },
                {
                    "state": "failure",
                    "matcher": "pathAny",
                    "argument": "Stacks[].StackStatus",
                    "expected": "UPDATE_ROLLBACK_FAILED",
                },
            ],
        },
    },
    "dynamodb": {
        "TableExists": {
            "operation": "DescribeTable",
            "delay": 20,
            "maxAttempts": 25,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "path",
                    "argument": "Table.TableStatus",
                    "expected": "ACTIVE",
                },
                {
                    "state": "retry",
                    "matcher": "error",
                    "expected": "ResourceNotFoundException",
                },
            ],
        },
        "TableNotExists": {
            "operation": "DescribeTable",
            "delay": 20,
            "maxAttempts": 25,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "error",
                    "expected": "ResourceNotFoundException",
                }
            ],
        },
    },
    "kinesis": {
        "StreamExists": {
            "operation": "DescribeStream",
            "delay": 10,
            "maxAttempts": 18,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "path",
                    "argument": "StreamDescription.StreamStatus",
                    "expected": "ACTIVE",
                }
            ],
        },
        "StreamNotExists": {
            "operation": "DescribeStream",
            "delay": 10,
            "maxAttempts": 18,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "error",
                    "expected": "ResourceNotFoundException",
                }
            ],
        },
    },
    "lambda": {
        "FunctionExists": {
            "operation": "GetFunction",
            "delay": 1,
            "maxAttempts": 20,
            "acceptors": [
                {"state": "success", "matcher": "status", "expected": 200},
                {
                    "state": "retry",
                    "matcher": "error",
                    "expected": "ResourceNotFoundException",
                },
            ],
        },
        "FunctionActive": {
            "operation": "GetFunctionConfiguration",
            "delay": 5,
            "maxAttempts": 60,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "path",
                    "argument": "State",
                    "expected": "Active",
                },
                {
                    "state": "failure",
                    "matcher": "path",
                    "argument": "State",
                    "expected": "Failed",
                },
                {
                    "state": "retry",
                    "matcher": "path",
                    "argument": "State",
                    "expected": "Pending",
                },
            ],
        },
        "FunctionActiveV2": {
            "operation": "GetFunction",
            "delay": 1,
            "maxAttempts": 300,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "path",
                    "argument": "Configuration.State",
                    "expected": "Active",
                },
                {
                    "state": "failure",
                    "matcher": "path",
                    "argument": "Configuration.State",
                    "expected": "Failed",
                },
                {
                    "state": "retry",
                    "matcher": "path",
                    "argument": "Configuration.State",
                    "expected": "Pending",
                },
            ],
        },
        "FunctionUpdated": {
            "operation": "GetFunctionConfiguration",
            "delay": 5,
            "maxAttempts": 60,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "path",
                    "argument": "LastUpdateStatus",
                    "expected": "Successful",
                },
                {
                    "state": "failure",
                    "matcher": "path",
                    "argument": "LastUpdateStatus",
                    "expected": "Failed",
                },
                {
                    "state": "retry",
                    "matcher": "path",
                    "argument": "LastUpdateStatus",
                    "expected": "InProgress",
                },
            ],
        },
        "FunctionUpdatedV2": {
            "operation": "GetFunction",
            "delay": 1,
            "maxAttempts": 300,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "path",
                    "argument": "Configuration.LastUpdateStatus",
                    "expected": "Successful",
                },
                {
                    "state": "failure",
                    "matcher": "path",
                    "argument": "Configuration.LastUpdateStatus",
                    "expected": "Failed",
                },
                {
                    "state": "retry",
                    "matcher": "path",
                    "argument": "Configuration.LastUpdateStatus",
                    "expected": "InProgress",
                },
            ],
        },
        "PublishedVersionActive": {
            "operation": "GetFunctionConfiguration",
            "delay": 5,
            "maxAttempts": 312,
            "acceptors": [
                {
                    "state": "success",
                    "matcher": "path",
                    "argument": "State",
                    "expected": "Active",
                },
                {
                    "state": "failure",
                    "matcher": "path",
                    "argument": "State",
                    "expected": "Failed",
                },
                {
                    "state": "retry",
                    "matcher": "path",
                    "argument": "State",
                    "expected": "Pending",
                },
            ],
        },
    },
    "s3": {
        "BucketExists": {
            "operation": "HeadBucket",
            "delay": 5,
            "maxAttempts": 20,
            "acceptors": [
                {"state": "success", "matcher": "status", "expected": 200},
                {"state": "success", "matcher": "status", "expected": 301},
                {"state": "success", "matcher": "status", "expected": 403},
                {"state": "retry", "matcher": "status", "expected": 404},
            ],
        },
        "BucketNotExists": {
            "operation": "HeadBucket",
            "delay": 5,
            "maxAttempts": 20,
            "acceptors": [{"state": "success", "matcher": "status", "expected": 404}],
        },
        "ObjectExists": {
            "operation": "HeadObject",
            "delay": 5,
            "maxAttempts": 20,
            "acceptors": [
                {"state": "success", "matcher": "status", "expected": 200},
                {"state": "retry", "matcher": "status", "expected": 404},
            ],
        },
        "ObjectNotExists": {
            "operation": "HeadObject",
            "delay": 5,
            "maxAttempts": 20,
            "acceptors": [{"state": "success", "matcher": "status", "expected": 404}],
        },
    },
}


def _status_of(response):
    """The HTTP status one answer came back with, which a `status` acceptor judges."""
    metadata = response.get("ResponseMetadata")
    if not isinstance(metadata, dict):
        return None
    return metadata.get("HTTPStatusCode")


class AcceptorConfig:
    """One acceptor: the state it moves a wait to, and what it accepts."""

    def __init__(self, definition):
        self.state = definition["state"]
        self.matcher = definition["matcher"]
        self.expected = definition.get("expected")
        self.argument = definition.get("argument")

    @property
    def explanation(self):
        """What this acceptor matched, as a `WaiterError` reports it."""
        if self.matcher == "path":
            return 'For expression "%s" we matched expected path: "%s"' % (
                self.argument,
                self.expected,
            )
        if self.matcher == "pathAll":
            return 'For expression "%s" all members matched expected path: "%s"' % (
                self.argument,
                self.expected,
            )
        if self.matcher == "pathAny":
            return 'For expression "%s" we matched expected path: "%s" at least once' % (
                self.argument,
                self.expected,
            )
        if self.matcher == "status":
            return "Matched expected HTTP status code: %s" % (self.expected,)
        return "Matched expected service error code: %s" % (self.expected,)

    def matches(self, response):
        """Whether this acceptor accepts one answer."""
        if self.matcher == "path":
            return search_path(response, self.argument) == self.expected
        if self.matcher == "pathAll":
            found = search_path(response, self.argument)
            if not isinstance(found, list) or not found:
                return False
            return all(element == self.expected for element in found)
        if self.matcher == "pathAny":
            found = search_path(response, self.argument)
            if not isinstance(found, list):
                return False
            return any(element == self.expected for element in found)
        if self.matcher == "status":
            return _status_of(response) == self.expected
        if self.matcher == "error":
            # The error matcher names a code, or asks whether the answer was a refusal at
            # all: `True` accepts any refusal and `False` accepts an answer that is none.
            error = response.get("Error")
            if self.expected is True:
                return isinstance(error, dict) and "Code" in error
            if self.expected is False:
                return error is None
            return isinstance(error, dict) and error.get("Code", "") == self.expected
        return False


class SingleWaiterConfig:
    """One waiter's own definition: what it polls, how it waits, and what it accepts."""

    def __init__(self, name, definition):
        self.name = name
        self.operation = definition["operation"]
        self.delay = definition.get("delay", 0)
        self.max_attempts = definition.get("maxAttempts", 1)
        self.acceptors = [
            AcceptorConfig(each) for each in definition.get("acceptors") or []
        ]

    @property
    def argument(self):
        """The argument the first acceptor judges, which the SDK reports here too."""
        for acceptor in self.acceptors:
            if acceptor.argument:
                return acceptor.argument
        return None


class WaiterModel:
    """The waiter definitions of ONE service, as the SDK's own model holds them."""

    def __init__(self, waiter_config):
        self._waiter_config = waiter_config.get("waiters") or {}
        self.waiter_names = sorted(self._waiter_config)

    def get_waiter(self, waiter_name):
        """The definition of one waiter, by the name the data states it under."""
        held = self._waiter_config.get(waiter_name)
        if held is None:
            raise ValueError("Waiter does not exist: %s" % (waiter_name,))
        return SingleWaiterConfig(waiter_name, held)


class NormalizedOperationMethod:
    """One poll: the client's own method, with a refusal handed back as its document.

    An acceptor judges the ANSWER, and a waiter's data matches a refusal (on its error code
    or its status) as readily as a served answer, so a refusal's own response document is
    what a poll hands back rather than an exception the wait could not judge.
    """

    def __init__(self, client_method):
        self._client_method = client_method

    def __call__(self, **kwargs):
        try:
            return self._client_method(**kwargs)
        except ClientError as error:
            return error.response


class Waiter:
    """One waiter: polls its operation until an acceptor decides the wait."""

    def __init__(self, name, config, operation_method):
        self.name = name
        self.config = config
        self._operation_method = operation_method

    def wait(self, **kwargs):
        """Poll until the state this waiter waits for, or until it gives up.

        The input is the polled operation's own, which every poll carries; a `WaiterConfig`
        the caller passes replaces this waiter's own `Delay` and `MaxAttempts` for this one
        wait. A success acceptor ends the wait, a failure acceptor and a wait that runs out
        of attempts raise `WaiterError`, and a refusal no acceptor names raises it too.
        """
        acceptors = list(self.config.acceptors)
        current_state = "waiting"
        sleep_amount = self.config.delay
        max_attempts = self.config.max_attempts
        asked = kwargs.pop("WaiterConfig", None) or {}
        if asked.get("Delay") is not None:
            sleep_amount = asked["Delay"]
        if asked.get("MaxAttempts") is not None:
            max_attempts = int(asked["MaxAttempts"])
        attempts = 0
        last_matched = None
        while True:
            response = self._operation_method(**kwargs)
            attempts += 1
            matched = None
            for acceptor in acceptors:
                if acceptor.matches(response):
                    matched = acceptor
                    break
            if matched is not None:
                last_matched = matched
                current_state = matched.state
            elif isinstance(response.get("Error"), dict):
                # A refusal no acceptor names is not something to poll through: the wait
                # ends on it, carrying the service's own code and sentence.
                error = response["Error"]
                raise WaiterError(
                    name=self.name,
                    reason="An error occurred (%s): %s"
                    % (
                        error.get("Code", "Unknown"),
                        error.get("Message", "Unknown"),
                    ),
                    last_response=response,
                )
            if current_state == "success":
                return
            if current_state == "failure":
                raise WaiterError(
                    name=self.name,
                    reason="Waiter encountered a terminal failure state: %s"
                    % (matched.explanation,),
                    last_response=response,
                )
            if attempts >= max_attempts:
                if last_matched is None:
                    reason = "Max attempts exceeded"
                else:
                    reason = "Max attempts exceeded. Previously accepted state: %s" % (
                        last_matched.explanation,
                    )
                raise WaiterError(name=self.name, reason=reason, last_response=response)
            # The wait between polls, which the runtime clamps to what the invocation has
            # left, so no wait outlasts the run it is made in.
            time.sleep(sleep_amount)


def service_waiters(service_name):
    """The waiter definitions one service publishes, as the SDK's model takes them."""
    return {"version": 2, "waiters": WAITERS.get(service_name) or {}}


def waiter_names(service_name):
    """The waiters a client for this service has, under the names the SDK reports."""
    return sorted(xform_name(name) for name in WAITERS.get(service_name) or {})


def create_waiter_from_legacy(waiter_name, waiter_model, client):
    """The waiter `waiter_name` of `waiter_model`, polling through `client`."""
    config = waiter_model.get_waiter(waiter_name)
    return Waiter(
        waiter_name,
        config,
        NormalizedOperationMethod(getattr(client, xform_name(config.operation))),
    )


def waiter_for(client, service_name, waiter_name):
    """The waiter a client answers `get_waiter(...)` with.

    The name is the one the SDK reports a waiter under (`table_exists` for the data's own
    `TableExists`); a name the service publishes no waiter for does not exist, which is what
    the SDK says rather than polling something else.
    """
    model = WaiterModel(service_waiters(service_name))
    named = {}
    for name in model.waiter_names:
        named[xform_name(name)] = name
    if waiter_name not in named:
        raise ValueError("Waiter does not exist: %s" % (waiter_name,))
    return create_waiter_from_legacy(named[waiter_name], model, client)
"#;
