// SPDX-License-Identifier: CC0-1.0

//! How the cluster's endpoint REFUSES a command, in the form its clients read.
//!
//! The engine answers a command it cannot serve with a reply document carrying `ok: 0`, an
//! `errmsg`, a numeric `code` and the `codeName` of that code — never with a closed
//! connection and never with a success. Every refusal this plane answers is built here, so
//! one code cannot be spelled two ways, and the codes are the engine's own: a driver maps
//! them to its exception types, so a wrong code is a wrong exception in the client.

/// One refusal, as the reply document it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireFault {
    /// The numeric `code`.
    pub code: i32,
    /// The `codeName` of that code, which is the name the engine publishes for it.
    pub code_name: &'static str,
    /// The `errmsg` sentence.
    pub message: String,
}

impl WireFault {
    /// The reply document a refusal is answered with.
    #[must_use]
    pub fn to_reply(&self) -> ::bson::Document {
        ::bson::doc! {
            "ok": 0.0_f64,
            "errmsg": self.message.clone(),
            "code": self.code,
            "codeName": self.code_name,
        }
    }
}

/// A value that is of the right type but outside what the command accepts (`BadValue`).
#[must_use]
pub fn bad_value(message: impl Into<String>) -> WireFault {
    WireFault {
        code: 2,
        code_name: "BadValue",
        message: message.into(),
    }
}

/// A command document that could not be read as the command it names (`FailedToParse`).
#[must_use]
pub fn failed_to_parse(message: impl Into<String>) -> WireFault {
    WireFault {
        code: 9,
        code_name: "FailedToParse",
        message: message.into(),
    }
}

/// A command a session may not run as whoever it is (`Unauthorized`).
#[must_use]
pub fn unauthorized(command: &str) -> WireFault {
    WireFault {
        code: 13,
        code_name: "Unauthorized",
        message: format!("command {command} requires authentication"),
    }
}

/// A SASL exchange that did not hold (`AuthenticationFailed`). The sentence is the engine's
/// own, and says nothing about which part did not hold.
#[must_use]
pub fn authentication_failed() -> WireFault {
    WireFault {
        code: 18,
        code_name: "AuthenticationFailed",
        message: "Authentication failed.".to_string(),
    }
}

/// A command naming a namespace this cluster does not hold (`NamespaceNotFound`).
#[must_use]
pub fn namespace_not_found(namespace: &str) -> WireFault {
    WireFault {
        code: 26,
        code_name: "NamespaceNotFound",
        message: format!("ns not found: {namespace}"),
    }
}

/// A `create` of a collection the database already has (`NamespaceExists`).
#[must_use]
pub fn namespace_exists(namespace: &str) -> WireFault {
    WireFault {
        code: 48,
        code_name: "NamespaceExists",
        message: format!("Collection {namespace} already exists."),
    }
}

/// A `getMore` or `killCursors` naming a cursor this connection does not hold
/// (`CursorNotFound`).
#[must_use]
pub fn cursor_not_found(cursor_id: i64, namespace: &str) -> WireFault {
    WireFault {
        code: 43,
        code_name: "CursorNotFound",
        message: format!("cursor id {cursor_id} not found on namespace {namespace}"),
    }
}

/// A command this endpoint does not serve (`CommandNotFound`). The engine's own sentence
/// names the command, so a driver's probe for an optional command reads as the absence of
/// that command rather than as a broken server.
#[must_use]
pub fn command_not_found(command: &str) -> WireFault {
    WireFault {
        code: 59,
        code_name: "CommandNotFound",
        message: format!("no such command: '{command}'"),
    }
}

/// A write that a unique index already holds a key for (`DuplicateKey`).
///
/// The sentence is the engine's: `E11000 duplicate key error collection: <ns> index: <index>
/// dup key: { <field>: <value> }`.
#[must_use]
pub fn duplicate_key(namespace: &str, index: &str, key: &::bson::Document) -> WireFault {
    WireFault {
        code: 11000,
        code_name: "DuplicateKey",
        message: format!(
            "E11000 duplicate key error collection: {namespace} index: {index} dup key: {}",
            super::query::render_key(key)
        ),
    }
}

/// A write sent to an endpoint that serves reads only — a cluster member that is not the one
/// the cluster writes through (`NotWritablePrimary`).
#[must_use]
pub fn not_writable_primary() -> WireFault {
    WireFault {
        code: 10107,
        code_name: "NotWritablePrimary",
        message: "not primary".to_string(),
    }
}

/// A command that is well formed but cannot be served in the state it was sent in
/// (`IllegalOperation`).
#[must_use]
pub fn illegal_operation(message: impl Into<String>) -> WireFault {
    WireFault {
        code: 20,
        code_name: "IllegalOperation",
        message: message.into(),
    }
}

/// A command refused by one of THIS emulator's own bounds on how much a cluster's volume
/// holds. The sentence names the bound, so the client can tell it from a defect.
#[must_use]
pub fn over_capacity(message: impl Into<String>) -> WireFault {
    WireFault {
        code: 8,
        code_name: "UnknownError",
        message: message.into(),
    }
}

/// A command this endpoint stopped waiting for: `MaxTimeMSExpired`, the engine's own code for
/// an operation that ran past the time allowed it.
#[must_use]
pub fn timed_out(milliseconds: u128) -> WireFault {
    WireFault {
        code: 50,
        code_name: "MaxTimeMSExpired",
        message: format!("operation exceeded time limit of {milliseconds} ms"),
    }
}
