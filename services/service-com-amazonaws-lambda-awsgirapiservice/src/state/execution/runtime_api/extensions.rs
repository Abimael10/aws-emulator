// SPDX-License-Identifier: CC0-1.0

//! The EXTENSIONS API of the runtime-API interface: the endpoints an extension — or a handler
//! that addresses them — registers at and reports its own failures to.
//!
//! An extension's whole relationship with the execution environment starts with ONE call:
//! `POST /2020-01-01/extension/register` names the extension in a header, names the lifecycle
//! events it wants in its body, and is answered with the identifier every later request of
//! that extension carries, beside the function the environment is running. That registration
//! is state, so it is minted against the invocation's own sandbox
//! ([`super::sandbox::Sandbox`]) and read back by every later call: a request carrying an
//! identifier this sandbox minted nothing for is from an extension that is not registered
//! here, and is refused as one.
//!
//! The event pump, `GET /2020-01-01/extension/event/next`, is reached here but never ANSWERED.
//! It is a LONG POLL for the next lifecycle event of the execution environment, and AWS
//! dispatches the invoke event of one invocation to the extensions registered BEFORE that
//! invocation began. This emulator runs no extension process — an extension is a program in
//! the deployment's own `/opt/extensions`, and nothing here starts one — so every registration
//! made in this sandbox is made from inside the invocation already in flight, after its event
//! was dispatched, and the next event of the environment is one no registration here outlives.
//! So that poll is reported as the request it is: one that reaches the interface and is never
//! answered ([`super::Reached::NeverAnswered`]), rather than one answered with an event nothing
//! dispatched.

use ::serde_json::{json, Value};

use super::{errored, header, json_answer, Reached, Requested};

/// Where an extension registers.
pub const REGISTER_PATH: &str = "/2020-01-01/extension/register";

/// Where it waits for the environment's next lifecycle event.
pub const EVENT_NEXT_PATH: &str = "/2020-01-01/extension/event/next";

/// Where it reports a failure of its own initialization.
pub const INIT_ERROR_PATH: &str = "/2020-01-01/extension/init/error";

/// Where it reports a failure on its way out.
pub const EXIT_ERROR_PATH: &str = "/2020-01-01/extension/exit/error";

/// The header the register call names the extension in: "the full file name of the
/// extension".
const NAME_HEADER: &str = "Lambda-Extension-Name";

/// The header every later request of a registered extension carries, and the one the register
/// call answers with: "generated unique agent identifier (UUID string) required for all
/// subsequent requests".
pub const IDENTIFIER_HEADER: &str = "Lambda-Extension-Identifier";

/// The header an error report states the class of its failure in.
const ERROR_TYPE_HEADER: &str = "Lambda-Extension-Function-Error-Type";

/// The lifecycle events a registration may ask for.
///
/// `INVOKE` and `SHUTDOWN` are the two the Extensions API defines; an extension that asks for
/// neither (`"events": []`) is registered all the same, which is what a telemetry-only
/// extension does.
const EVENTS: &[&str] = &["INVOKE", "SHUTDOWN"];

/// What the interface answers `request`, for a request addressed at one of this API's paths —
/// and [`Reached::Elsewhere`] for a path that is none of them.
pub fn answered(request: &Requested<'_>, path: &str) -> Reached {
    match (request.method, path) {
        ("POST", REGISTER_PATH) => registered(request),
        ("GET", EVENT_NEXT_PATH) => next_event(request),
        ("POST", INIT_ERROR_PATH | EXIT_ERROR_PATH) => reported(request),
        _ => Reached::Elsewhere,
    }
}

/// The answer to a register call: the identifier minted for the extension, and the function
/// whose execution environment it registered in.
fn registered(request: &Requested<'_>) -> Reached {
    let Some(sandbox) = request.sandbox else {
        return Reached::Answered(no_sandbox());
    };
    let Some(name) = header(request.headers, NAME_HEADER)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        // The extension is named in that header, and a request that names no extension
        // registers none: AWS answers the register call `403` for a request it will not
        // register.
        return Reached::Answered(errored(
            403,
            "InvalidRequest",
            &format!("The {NAME_HEADER} header is missing from the request"),
        ));
    };
    let events = match events_asked_for(request.body) {
        Ok(events) => events,
        Err(message) => return Reached::Answered(errored(400, "InvalidRequestFormat", &message)),
    };
    let identifier = sandbox.register(name, events);
    let facts = sandbox.facts();
    let mut answer = json_answer(
        200,
        &json!({
            "functionName": facts.function_name,
            "functionVersion": facts.function_version,
            "handler": facts.handler,
            "accountId": facts.account_id,
        }),
    );
    answer
        .headers
        .push((IDENTIFIER_HEADER.to_string(), identifier));
    Reached::Answered(answer)
}

/// The events a register body asks for, or the detail of what is wrong with it.
///
/// A body that is not there at all asks for no event, which is a registration all the same —
/// the position a telemetry-only extension registers from.
fn events_asked_for(body: &[u8]) -> Result<Vec<String>, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let document: Value = ::serde_json::from_slice(body)
        .map_err(|error| format!("The request body is not valid JSON: {error}"))?;
    let Some(asked) = document.get("events") else {
        return Ok(Vec::new());
    };
    let Some(asked) = asked.as_array() else {
        return Err("The events field of the request body is not a list".to_string());
    };
    let mut events = Vec::with_capacity(asked.len());
    for event in asked {
        let named = event.as_str().unwrap_or_default();
        if !EVENTS.contains(&named) {
            return Err(format!("Unknown event type: {named}"));
        }
        events.push(named.to_string());
    }
    Ok(events)
}

/// The answer to a poll for the environment's next lifecycle event: none, ever (see the module
/// docs). The registration itself is still read, so an extension that is not registered here
/// is refused as one rather than left waiting.
fn next_event(request: &Requested<'_>) -> Reached {
    match registered_caller(request) {
        Ok(_caller) => Reached::NeverAnswered,
        Err(refusal) => Reached::Answered(refusal),
    }
}

/// The answer to an extension's own error report: accepted, which is what there is to say
/// about a failure the extension itself reports.
fn reported(request: &Requested<'_>) -> Reached {
    if let Err(refusal) = registered_caller(request) {
        return Reached::Answered(refusal);
    }
    if header(request.headers, ERROR_TYPE_HEADER)
        .map(str::trim)
        .filter(|class| !class.is_empty())
        .is_none()
    {
        return Reached::Answered(errored(
            400,
            "InvalidRequestFormat",
            &format!("The {ERROR_TYPE_HEADER} header is missing from the request"),
        ));
    }
    Reached::Answered(json_answer(202, &json!({ "status": "OK" })))
}

/// WHO one request of a registered extension is from: the sandbox it is in, and the
/// registration it carries the identifier of.
#[derive(Debug)]
pub struct Caller<'a> {
    /// The execution environment the request was made from.
    pub sandbox: &'a super::Sandbox,
    /// The identifier the request named itself with, which that sandbox minted.
    pub identifier: &'a str,
}

/// The REGISTERED extension a request is from, or the refusal for one that is from none: the
/// identifier is what says which extension is calling, so a request carrying none, or one this
/// sandbox minted nothing for, is from no extension registered here.
pub fn registered_caller<'a>(request: &Requested<'a>) -> Result<Caller<'a>, super::Answered> {
    let Some(sandbox) = request.sandbox else {
        return Err(no_sandbox());
    };
    let identifier = header(request.headers, IDENTIFIER_HEADER)
        .map(str::trim)
        .filter(|identifier| !identifier.is_empty());
    let Some(identifier) = identifier else {
        return Err(errored(
            403,
            "InvalidRequest",
            &format!("The {IDENTIFIER_HEADER} header is missing from the request"),
        ));
    };
    if sandbox.registration(identifier).is_none() {
        return Err(errored(
            403,
            "InvalidRequest",
            &format!("Unknown extension {identifier}"),
        ));
    }
    Ok(Caller {
        sandbox,
        identifier,
    })
}

/// The answer where the request reached this interface from outside any sandbox of this
/// emulator — no invocation is running, so there is no execution environment to register in.
///
/// "500 – Container error. Non-recoverable state. Extension should exit promptly" is what the
/// Extensions API says about a sandbox that cannot serve the call at all, which is this.
fn no_sandbox() -> super::Answered {
    errored(
        500,
        "ContainerError",
        "No execution environment is serving this request",
    )
}
