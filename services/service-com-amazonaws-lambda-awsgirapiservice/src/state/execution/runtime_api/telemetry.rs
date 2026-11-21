// SPDX-License-Identifier: CC0-1.0

//! The TELEMETRY API of the runtime-API interface: where a registered extension SUBSCRIBES to
//! the streams of the execution environment's telemetry.
//!
//! One call carries the whole subscription: which schema the events are rendered in, which
//! streams are wanted, where they are delivered and how they are buffered. The call names the
//! extension it is from in a header, and that registration is read first — the subscription is
//! held against it. Then the API READS the document into those members before it judges
//! anything, so a body it cannot read is refused as one that could not be deserialized —
//! naming the member it stopped on, what it found there and what belongs there, with the
//! position in the document it stopped at — and a body it can read is then judged against the
//! rules its members carry, which are stated together: a request that breaks several of them is
//! answered with each failure in turn, separated by `"; "`.
//!
//! Both of those are what the TYPES below are for: the document is deserialized into them, so
//! the reading, the values each member admits and the detail of a body that states something
//! else are the deserializer's own rather than a hand-written scan of the caller's JSON.
//!
//! A body that satisfies every rule is recorded as the subscription of the registration that
//! made it ([`super::sandbox::Subscription`]) and answered `OK`. Nothing is delivered to a
//! destination nothing answers at: the destination of a telemetry subscription is an endpoint
//! the extension itself listens on inside the execution environment, and this emulator runs no
//! extension process to listen on one, so a subscription recorded here has exactly the
//! delivery AWS gives one whose destination answers nothing — none — and no event is reported
//! as delivered anywhere.

use ::serde::Deserialize;
use ::serde_json::json;

use super::sandbox::{Buffering, Subscription};
use super::{errored, json_answer, Reached, Requested};

/// Where a registered extension subscribes to the telemetry streams.
pub const SUBSCRIBE_PATH: &str = "/2022-07-01/telemetry";

/// The class a body the API could not READ is refused under.
const DESERIALIZATION_ERROR: &str = "Telemetry.DeserializationError";

/// The class a body the API read but will not accept is refused under.
const VALIDATION_ERROR: &str = "Telemetry.ValidationError";

/// The status either refusal carries.
const REFUSED_STATUS: u16 = 400;

/// The SCHEMA one subscription's events are rendered in.
///
/// Every version this interface has published, oldest first — the two of the logs stream it
/// grew out of among them, because a subscription may still ask for them.
#[derive(Debug, Clone, Copy, Deserialize)]
enum SchemaVersion {
    /// The first version of the logs subscription this interface grew out of.
    #[serde(rename = "2020-08-15")]
    Logs20200815,
    /// Its second version.
    #[serde(rename = "2021-03-18")]
    Logs20210318,
    /// The first version of the telemetry subscription.
    #[serde(rename = "2022-07-01")]
    Telemetry20220701,
    /// Its second version.
    #[serde(rename = "2022-12-13")]
    Telemetry20221213,
    /// Its third version.
    #[serde(rename = "2025-01-29")]
    Telemetry20250129,
}

impl SchemaVersion {
    /// The version as the subscription spelled it, which is how it is recorded.
    fn spelling(self) -> &'static str {
        match self {
            Self::Logs20200815 => "2020-08-15",
            Self::Logs20210318 => "2021-03-18",
            Self::Telemetry20220701 => "2022-07-01",
            Self::Telemetry20221213 => "2022-12-13",
            Self::Telemetry20250129 => "2025-01-29",
        }
    }
}

/// ONE telemetry stream a subscription may ask for.
#[derive(Debug, Clone, Copy, Deserialize)]
enum Stream {
    /// The records the platform itself writes about the lifecycle of the environment.
    #[serde(rename = "platform")]
    Platform,
    /// The records the function's own code writes.
    #[serde(rename = "function")]
    Function,
    /// The records an extension writes.
    #[serde(rename = "extension")]
    Extension,
}

impl Stream {
    /// The stream as the subscription spelled it.
    fn spelling(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Function => "function",
            Self::Extension => "extension",
        }
    }
}

/// The PROTOCOL a destination is reached over.
#[derive(Debug, Clone, Copy, Deserialize)]
enum Protocol {
    /// The one protocol a telemetry destination is delivered to over.
    #[serde(rename = "HTTP")]
    Http,
}

impl Protocol {
    /// The protocol as the subscription spelled it.
    fn spelling(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
        }
    }
}

/// WHERE one subscription's events are delivered.
#[derive(Debug, Clone, Deserialize)]
struct Destination {
    /// The protocol the destination is reached over.
    protocol: Protocol,
    /// The destination itself, which the extension listens on.
    #[serde(rename = "URI")]
    uri: String,
}

/// How a subscription asked for its delivery to be buffered, before the bounds of each member
/// are read and the ones it left out take their defaults.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct AskedBuffering {
    /// The most events one delivery is to carry.
    #[serde(rename = "maxItems")]
    max_items: Option<u64>,
    /// The most bytes one delivery is to carry.
    #[serde(rename = "maxBytes")]
    max_bytes: Option<u64>,
    /// How long events are to be held before one delivery is made.
    #[serde(rename = "timeoutMs")]
    timeout_ms: Option<u64>,
}

/// The whole body of a subscribe call, as the API reads it.
#[derive(Debug, Clone, Deserialize)]
struct Subscribing {
    /// The schema its events are rendered in.
    #[serde(rename = "schemaVersion")]
    schema_version: SchemaVersion,
    /// The streams it asks for.
    types: Vec<Stream>,
    /// Where its events are delivered.
    destination: Destination,
    /// How the delivery is buffered, for a subscription that asked.
    #[serde(default)]
    buffering: Option<AskedBuffering>,
}

/// The bounds on how many events one delivery may carry, and what the member is when the
/// subscription leaves it out.
const MAX_ITEMS: Bound = Bound {
    member: "maxItems",
    low: 1_000,
    high: 10_000,
    default: 1_000,
};

/// The bounds on how many bytes one delivery may carry.
const MAX_BYTES: Bound = Bound {
    member: "maxBytes",
    low: 262_144,
    high: 1_048_576,
    default: 262_144,
};

/// The bounds on how long events are held before one delivery is made.
const TIMEOUT_MS: Bound = Bound {
    member: "timeoutMs",
    low: 25,
    high: 30_000,
    default: 1_000,
};

/// One member of `buffering`: what it is called, the range it is published with, and the value
/// a subscription that leaves it out gets.
struct Bound {
    /// The member's own name, as the body spells it.
    member: &'static str,
    /// The smallest value it may take.
    low: u64,
    /// The largest value it may take.
    high: u64,
    /// What it is when the body leaves it out.
    default: u64,
}

/// What the interface answers `request`, for a request addressed at this API's path — and
/// [`Reached::Elsewhere`] for a path that is not it.
pub fn answered(request: &Requested<'_>, path: &str) -> Reached {
    if (request.method, path) != ("PUT", SUBSCRIBE_PATH) {
        return Reached::Elsewhere;
    }
    // WHICH extension is subscribing is read FIRST: the subscription is held against the
    // registration that made it, so a request from an extension this sandbox registered none
    // of subscribes nothing, whatever its body states.
    let caller = match super::extensions::registered_caller(request) {
        Ok(caller) => caller,
        Err(refusal) => return Reached::Answered(refusal),
    };
    let subscription = match subscribed(request.body) {
        Ok(subscription) => subscription,
        Err((class, message)) => {
            return Reached::Answered(errored(REFUSED_STATUS, class, &message))
        }
    };
    if !caller.sandbox.subscribe(caller.identifier, subscription) {
        return Reached::Answered(errored(
            403,
            "InvalidRequest",
            &format!("Unknown extension {}", caller.identifier),
        ));
    }
    // "If your Subscribe request succeeds, you receive an HTTP 200 success response": the
    // body of it is the word the API answers, not a document of members.
    Reached::Answered(json_answer(200, &json!("OK")))
}

/// The subscription a body states: its class and detail where it states none.
///
/// Two readings, in the order the API makes them. The document is DESERIALIZED into the
/// members above — so a member that is missing, holds something of another type, or names a
/// value the member does not publish is reported as the deserializer reads it, naming the
/// position in the document it stopped at. Then the rules the members carry beyond their own
/// types are judged, all of them: a body that breaks several is answered with each failure in
/// turn.
fn subscribed(body: &[u8]) -> Result<Subscription, (&'static str, String)> {
    let asked: Subscribing = ::serde_json::from_slice(body)
        .map_err(|error| (DESERIALIZATION_ERROR, error.to_string()))?;
    let mut failures = Vec::new();
    if asked.types.is_empty() {
        failures.push("types should not be empty".to_string());
    }
    // A destination is delivered to over HTTP, so the port it is reached on is part of it.
    if port_of(&asked.destination.uri).is_none() {
        failures.push("URI port is not provided".to_string());
    }
    let buffering = Buffering {
        max_items: bounded(
            asked.buffering,
            |asked| asked.max_items,
            &MAX_ITEMS,
            &mut failures,
        ),
        max_bytes: bounded(
            asked.buffering,
            |asked| asked.max_bytes,
            &MAX_BYTES,
            &mut failures,
        ),
        timeout_ms: bounded(
            asked.buffering,
            |asked| asked.timeout_ms,
            &TIMEOUT_MS,
            &mut failures,
        ),
    };
    if !failures.is_empty() {
        return Err((VALIDATION_ERROR, failures.join("; ")));
    }
    Ok(Subscription {
        schema_version: asked.schema_version.spelling().to_string(),
        types: asked
            .types
            .iter()
            .map(|stream| stream.spelling().to_string())
            .collect(),
        protocol: asked.destination.protocol.spelling().to_string(),
        uri: asked.destination.uri,
        buffering,
    })
}

/// What one member of `buffering` is set to, reporting a value outside its published range as
/// a failure and taking the member's own default where the subscription stated none.
fn bounded(
    buffering: Option<AskedBuffering>,
    member: fn(&AskedBuffering) -> Option<u64>,
    bound: &Bound,
    failures: &mut Vec<String>,
) -> u64 {
    let Some(value) = buffering.as_ref().and_then(member) else {
        return bound.default;
    };
    if value < bound.low || value > bound.high {
        failures.push(format!(
            "{} should be between {} and {}",
            bound.member, bound.low, bound.high
        ));
        return bound.default;
    }
    value
}

/// The PORT a destination URI names, or `None` for one that names none — which the Telemetry
/// API requires of a destination.
///
/// The authority is located here — the value is the caller's STRING, which may be no URI at
/// all, and that has to come back as "no port" rather than as a parser's error — and then READ
/// by `http`'s own authority type, which this crate already depends on: the userinfo, the
/// bracketed form of an IPv6 address and what is a valid port are its reading rather than a
/// scan written here. (`http::Uri` itself is not the reader, because a destination may be
/// written as a bare `host:port` address, which as a URI is a scheme followed by a path.)
fn port_of(uri: &str) -> Option<u16> {
    let rest = uri.split_once("://").map_or(uri, |(_scheme, rest)| rest);
    let authority = match rest.find(['/', '?', '#']) {
        Some(at) => &rest[..at],
        None => rest,
    };
    authority
        .parse::<::http::uri::Authority>()
        .ok()?
        .port_u16()
}

#[cfg(test)]
mod tests {
    use ::serde_json::Value;

    use super::*;

    /// A body with every member stated, which each row then changes one member of.
    fn body(member: &str, value: Value) -> Vec<u8> {
        let mut document = json!({
            "schemaVersion": "2022-12-13",
            "types": ["platform", "function", "extension"],
            "destination": { "protocol": "HTTP", "URI": "http://sandbox.localdomain:8080/x" },
        });
        if value.is_null() {
            document
                .as_object_mut()
                .expect("an object")
                .remove(member);
        } else {
            document[member] = value;
        }
        ::serde_json::to_vec(&document).expect("the body must serialize")
    }

    /// Every member of the subscribe body is READ, and a body that states none is refused
    /// under the class of the reading that failed, with the detail of what it states.
    ///
    /// The rows differ along the member and along HOW it fails to state one: a member that is
    /// absent, one holding a value of another type, and one naming a value the member does not
    /// publish.
    #[test]
    fn a_body_that_states_no_subscription_is_refused_as_it_reads() {
        // (case, the body, the class it is refused under, what the detail must name)
        let rows: &[(&str, Vec<u8>, &str, &str)] = &[
            (
                "a schema version the member does not publish",
                body("schemaVersion", json!("9999-99-99")),
                DESERIALIZATION_ERROR,
                "unknown variant `9999-99-99`",
            ),
            (
                "no schema version at all",
                body("schemaVersion", Value::Null),
                DESERIALIZATION_ERROR,
                "missing field `schemaVersion`",
            ),
            (
                "a stream the member does not publish",
                body("types", json!(["platform", "audit"])),
                DESERIALIZATION_ERROR,
                "unknown variant `audit`",
            ),
            (
                "streams that are not a list",
                body("types", json!("platform")),
                DESERIALIZATION_ERROR,
                "invalid type: string \"platform\"",
            ),
            (
                "no destination at all",
                body("destination", Value::Null),
                DESERIALIZATION_ERROR,
                "missing field `destination`",
            ),
            (
                "a protocol the member does not publish",
                body(
                    "destination",
                    json!({ "protocol": "TCP", "URI": "http://sandbox.localdomain:8080/x" }),
                ),
                DESERIALIZATION_ERROR,
                "unknown variant `TCP`",
            ),
            (
                "no destination URI at all",
                body("destination", json!({ "protocol": "HTTP" })),
                DESERIALIZATION_ERROR,
                "missing field `URI`",
            ),
            (
                "a body that is no document at all",
                b"not a document".to_vec(),
                DESERIALIZATION_ERROR,
                "expected ident",
            ),
            (
                "no stream asked for",
                body("types", json!([])),
                VALIDATION_ERROR,
                "types should not be empty",
            ),
            (
                "a destination naming no port",
                body(
                    "destination",
                    json!({ "protocol": "HTTP", "URI": "http://sandbox.localdomain/x" }),
                ),
                VALIDATION_ERROR,
                "URI port is not provided",
            ),
            (
                "buffering one past the low edge of every bound",
                body(
                    "buffering",
                    json!({ "maxItems": 999, "maxBytes": 262_143, "timeoutMs": 24 }),
                ),
                VALIDATION_ERROR,
                "maxItems should be between 1000 and 10000; maxBytes should be between \
                 262144 and 1048576; timeoutMs should be between 25 and 30000",
            ),
            (
                "buffering one past the high edge of every bound",
                body(
                    "buffering",
                    json!({ "maxItems": 10_001, "maxBytes": 1_048_577, "timeoutMs": 30_001 }),
                ),
                VALIDATION_ERROR,
                "maxItems should be between 1000 and 10000; maxBytes should be between \
                 262144 and 1048576; timeoutMs should be between 25 and 30000",
            ),
        ];
        for (case, body, class, detail) in rows {
            let read = subscribed(body);
            let Err((refused, message)) = read else {
                panic!("case {case:?}: expected a refusal, got {read:?}");
            };
            assert_eq!(
                refused, *class,
                "case {case:?}: expected {class:?}, got {refused:?}"
            );
            assert!(
                message.contains(detail),
                "case {case:?}: expected a detail naming {detail:?}, got {message:?}"
            );
        }
    }

    /// A body that states a subscription is read into it, whichever published value each
    /// member takes.
    ///
    /// The rows span the published values the members carry — every schema version, each
    /// stream on its own, and the bounds of `buffering` at both edges — so what is asserted is
    /// that each of them IS a subscription rather than one example of one.
    #[test]
    fn a_body_that_states_a_subscription_is_read_into_it() {
        let mut rows: Vec<(String, Vec<u8>)> = Vec::new();
        // Every version the member publishes, which is the list its own refusal names.
        for version in [
            "2020-08-15",
            "2021-03-18",
            "2022-07-01",
            "2022-12-13",
            "2025-01-29",
        ] {
            rows.push((
                format!("the schema version {version}"),
                body("schemaVersion", json!(version)),
            ));
        }
        for stream in ["platform", "function", "extension"] {
            rows.push((
                format!("the stream {stream}"),
                body("types", json!([stream])),
            ));
        }
        for (edge, items, bytes, timeout) in [
            ("the low edge", MAX_ITEMS.low, MAX_BYTES.low, TIMEOUT_MS.low),
            (
                "the high edge",
                MAX_ITEMS.high,
                MAX_BYTES.high,
                TIMEOUT_MS.high,
            ),
        ] {
            rows.push((
                format!("buffering at {edge} of every bound"),
                body(
                    "buffering",
                    json!({ "maxItems": items, "maxBytes": bytes, "timeoutMs": timeout }),
                ),
            ));
        }
        for (case, body) in rows {
            let read = subscribed(&body);
            assert!(
                read.is_ok(),
                "case {case:?}: expected a subscription, got {read:?}"
            );
        }
    }

    /// The buffering a body leaves out is the default of each member, and the subscription
    /// records what the body stated.
    #[test]
    fn a_subscription_records_what_the_body_stated() {
        let subscription = subscribed(&body("schemaVersion", json!("2022-07-01")))
            .expect("the body must state a subscription");
        assert_eq!(
            subscription.buffering,
            Buffering {
                max_items: MAX_ITEMS.default,
                max_bytes: MAX_BYTES.default,
                timeout_ms: TIMEOUT_MS.default,
            },
            "case \"no buffering stated\": expected each member's default, got {:?}",
            subscription.buffering
        );
        assert_eq!(
            (
                subscription.schema_version.as_str(),
                subscription.types.as_slice(),
                subscription.protocol.as_str(),
                subscription.uri.as_str(),
            ),
            (
                "2022-07-01",
                [
                    "platform".to_string(),
                    "function".to_string(),
                    "extension".to_string()
                ]
                .as_slice(),
                "HTTP",
                "http://sandbox.localdomain:8080/x",
            ),
            "case \"a stated subscription\": expected the members the body stated, got \
             {subscription:?}"
        );
    }

    /// A destination URI names a port, however the URI is written.
    #[test]
    fn a_destination_uri_names_its_port() {
        // (case, the URI, the port it names)
        let rows: &[(&str, &str, Option<u16>)] = &[
            (
                "a host and a port",
                "http://sandbox.localdomain:8080/x",
                Some(8080),
            ),
            ("no path", "http://sandbox.localdomain:8080", Some(8080)),
            ("no scheme", "sandbox.localdomain:8080/x", Some(8080)),
            ("no port", "http://sandbox.localdomain/x", None),
            ("a bracketed address", "http://[::1]:8080/x", Some(8080)),
            ("a bracketed address with no port", "http://[::1]/x", None),
            (
                "a port that is no number",
                "http://sandbox.localdomain:http/x",
                None,
            ),
            ("a value that is no URI", "sandbox", None),
        ];
        for (case, uri, expected) in rows {
            let read = port_of(uri);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
