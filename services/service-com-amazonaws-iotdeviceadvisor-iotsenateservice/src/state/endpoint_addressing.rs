// SPDX-License-Identifier: CC0-1.0

//! WHERE a device under test CONNECTS: the address this emulator serves the Device Advisor
//! endpoint at — the one value `GetEndpoint` vends and a suite run reports and persists for
//! its primary device.
//!
//! `Endpoint` is "The response of an Device Advisor endpoint"
//! (`smithy_query(query="operation",operation="GetEndpoint")`), and what a client DOES with
//! that value is OPEN A CONNECTION to it: the device under test is handed the endpoint and
//! dials it, which is the only reason the operation exists. An endpoint a service announces
//! is therefore an address the service ANSWERS on, and the one such address this process has
//! is a socket of its own: a name in the zone AWS publishes its own endpoints in resolves
//! nowhere near this machine or, worse, at genuine AWS infrastructure, and a name this
//! emulator invents under any other zone resolves nowhere at all — either way the device is
//! sent off the emulator and every connection it opens fails.
//!
//! WHICH socket: a device under test reaches a Device Advisor endpoint with MQTT, so the
//! address reported is this emulator's MQTT MESSAGE BROKER
//! ([`::service_common::iot_mqtt_broker::ensure_broker_endpoint`]) — the one broker the IoT
//! services serve and report sessions on, opened on first use and kept for the life of its
//! store, whose socket serves both the MQTT a device dials and the HTTPS half the same
//! address carries. A device handed this value connects, and the session it opens is the one
//! the IoT control plane reports; this is the same address the IoT data endpoint a device
//! dials is published at (`service-com-amazonaws-iot-awsiotservice/src/ops/describe_endpoint.rs`).
//!
//! An emulator that can open no socket at all — one driven in process by a host, with no IO
//! behind it — serves the device wire nowhere, and then there IS no endpoint to report: the
//! operation answers its modelled server failure. The address of this emulator's own HTTP
//! front door is not an answer here, because nothing there speaks the wire a device opens, so
//! a device handed it would fail on the protocol instead of on the connection.
//!
//! The modelled `len 45..75` on `Endpoint` is the width of the hostname AWS answers with,
//! and no address of this process is that wide: a loopback authority is nine to twenty-one
//! characters, and padding one out to forty-five turns the host a client dials into a name
//! nothing answers at — which is the very defect this derivation exists to avoid. Where the
//! two cannot both hold, the member's purpose decides: the value is dialled, so what is
//! reported is the address this emulator answers on, and the width is knowingly not met.
//!
//! One derivation, so the address a run persists at its start and the address `GetEndpoint`
//! vends cannot disagree — and nothing of a request's own payload moves it: the endpoint is
//! the account's, not a device's, because an address cannot carry a label per device the way
//! AWS's generated hostname does, and a caller dials what it is told.

/// The address a reporting operation announces as the Device Advisor endpoint, OPENING the
/// socket that serves it on first use (idempotent: the first socket opened is the one kept,
/// so every later call reports the same address and a client may hold on to it).
///
/// `None` for an emulator that can open no socket at all, which serves the device wire
/// nowhere: the operation asking has no endpoint to report and answers its modelled server
/// failure rather than an address nothing a device speaks is served at.
///
/// `GetEndpoint` vends it and `StartSuiteRun` reports and persists it for the run's primary
/// device, both through this one derivation, so a client told where a device connects by the
/// start call is told the same address when it asks for that device's endpoint.
pub async fn announced_endpoint<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Option<String> {
    ::service_common::iot_mqtt_broker::ensure_broker_endpoint(engine, ctx.callback()).await
}

/// What an operation that can report no endpoint says: this emulator serves the wire a device
/// opens nowhere, so there is no address to hand back.
///
/// The message of the modelled server failure both reporting operations answer in that case,
/// written once here so the two cannot word it differently.
pub const NO_ENDPOINT_MESSAGE: &str =
    "the Device Advisor endpoint is unavailable in this emulator: no socket could be opened \
     to serve the device under test";
