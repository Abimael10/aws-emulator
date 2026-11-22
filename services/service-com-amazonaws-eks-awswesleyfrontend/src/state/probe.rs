// SPDX-License-Identifier: CC0-1.0

//! The health PROBES a pod's container declares, projected out of the Kubernetes
//! protobuf encoding onto the document the rest of this API server works in.
//!
//! A container declares up to three of them — `livenessProbe`, `readinessProbe`
//! and `startupProbe` — and each one is the same shape: the action the kubelet
//! performs (a command inside the container, an HTTP GET, a TCP connection, a gRPC
//! call) and the timings it performs it on. In the protobuf encoding the action is
//! EMBEDDED in the probe message, while a client writing JSON writes its members
//! at the probe's own level, so the projection flattens it — which is what makes
//! one container read back the same whichever encoding it arrived in
//! ([`super::kubernetes_protobuf`]).
//!
//! Its own module because a probe is the one member of a container that is a tree
//! of its own: four actions, the port each addresses — written either as a number
//! or as the name of a container port — and the headers an HTTP one sends.

use ::k8s_pb::api::core::v1 as core_v1;
use ::k8s_pb::apimachinery::pkg::util::intstr::IntOrString;
use ::serde_json::{Map, Value};

use super::kubernetes_protobuf::{put_number, put_text, put_texts, put_values};

/// One probe as the document a client reads it back as: the action the kubelet
/// performs and the timings it performs it on.
#[must_use]
pub(super) fn projected(probe: &core_v1::Probe) -> Value {
    let mut document = Map::new();
    if let Some(action) = &probe.handler {
        if let Some(exec) = &action.exec {
            let mut projected = Map::new();
            put_texts(&mut projected, "command", &exec.command);
            document.insert("exec".to_string(), Value::Object(projected));
        }
        if let Some(request) = &action.http_get {
            let mut projected = Map::new();
            put_text(&mut projected, "path", request.path.as_deref());
            if let Some(port) = request.port.as_ref().map(port_value) {
                projected.insert("port".to_string(), port);
            }
            put_text(&mut projected, "host", request.host.as_deref());
            put_text(&mut projected, "scheme", request.scheme.as_deref());
            let headers: Vec<Value> = request
                .http_headers
                .iter()
                .map(|header| {
                    let mut projected = Map::new();
                    put_text(&mut projected, "name", header.name.as_deref());
                    put_text(&mut projected, "value", header.value.as_deref());
                    Value::Object(projected)
                })
                .collect();
            put_values(&mut projected, "httpHeaders", headers);
            document.insert("httpGet".to_string(), Value::Object(projected));
        }
        if let Some(socket) = &action.tcp_socket {
            let mut projected = Map::new();
            if let Some(port) = socket.port.as_ref().map(port_value) {
                projected.insert("port".to_string(), port);
            }
            put_text(&mut projected, "host", socket.host.as_deref());
            document.insert("tcpSocket".to_string(), Value::Object(projected));
        }
        if let Some(call) = &action.grpc {
            let mut projected = Map::new();
            put_number(&mut projected, "port", call.port);
            put_text(&mut projected, "service", call.service.as_deref());
            document.insert("grpc".to_string(), Value::Object(projected));
        }
    }
    put_number(
        &mut document,
        "initialDelaySeconds",
        probe.initial_delay_seconds,
    );
    put_number(&mut document, "timeoutSeconds", probe.timeout_seconds);
    put_number(&mut document, "periodSeconds", probe.period_seconds);
    put_number(&mut document, "successThreshold", probe.success_threshold);
    put_number(&mut document, "failureThreshold", probe.failure_threshold);
    put_number(
        &mut document,
        "terminationGracePeriodSeconds",
        probe.termination_grace_period_seconds,
    );
    Value::Object(document)
}

/// A port an action addresses, as the document carries it: the NUMBER of a port,
/// or the NAME of a container port — the two forms the Kubernetes API takes there,
/// which the encoding tells apart by the `type` the value carries.
fn port_value(port: &IntOrString) -> Value {
    match port.str_val.as_deref() {
        Some(name) if !name.is_empty() => Value::String(name.to_string()),
        _ => Value::from(i64::from(port.int_val.unwrap_or_default())),
    }
}
