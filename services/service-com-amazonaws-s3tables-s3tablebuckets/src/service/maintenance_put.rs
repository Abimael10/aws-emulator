// SPDX-License-Identifier: CC0-1.0

//! The refusals a MAINTENANCE PUT states itself in front of dispatch: the task
//! its path addresses, and the configuration value its body must carry.
//!
//! Each maintenance put names the task it configures in the request PATH —
//! `/tables/{tableBucketARN}/{namespace}/{name}/maintenance/{type}` configures
//! one of a TABLE's tasks and `/buckets/{tableBucketARN}/maintenance/{type}` one
//! of a table BUCKET's — and carries the configuration itself in a `@required`
//! `value` member of its body (`smithy_query(query="operation",
//! operation="PutTableMaintenanceConfiguration,
//! PutTableBucketMaintenanceConfiguration")`). Two things a client gets wrong
//! there are refused by S3 Tables with a sentence of its own, under the one
//! client-input error its model declares:
//!
//! * a path naming a task the addressed resource does not have — the model
//!   enumerates them per resource — addresses no maintenance configuration at
//!   all, and the refusal states the type the path carried;
//! * a body carrying no configuration value addresses a configuration with
//!   nothing to set it to, and the refusal states that the value is not there.
//!
//! Both are made HERE, at the seam in front of dispatch
//! ([`smithy_server_runtime::Service::override_operation`], where
//! [`super::request_layer`] and [`super::name_lookups`] are also made), because
//! the framework would otherwise state each in its own words:
//!
//! * the type, in front of the layer that answers a body this service cannot
//!   read, because the settings a maintenance configuration carries are a union
//!   whose members are the tasks THEMSELVES — a request naming an unknown task
//!   usually carries that task's settings too, which read into no member of the
//!   addressed resource's union, so left to that layer the caller would be told
//!   about the body it sent where the path is what named no task;
//! * the absent value, because a member found absent while READING the request
//!   never becomes a violation of the walk this service words through
//!   [`crate::service::EmulatedService::override_refusal`]: it is reported per
//!   member by the framework, which is the right default for every other
//!   operation and is exactly what this one states a sentence for.
//!
//! Nothing about the surface is written down twice: WHICH tasks a resource has is
//! read off this service's own state enums, the values every handler and every
//! read of a configuration already uses; WHERE the type sits in the path is read
//! off the operation's modelled `@http` URI ([`super::maintenance_type`]); and
//! WHETHER the value is there is decided by running the framework's OWN
//! deserializer over the body, with the same protocol and the same input schema
//! the dispatch sequence uses.

use std::marker::PhantomData;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::SerdeError;
use smithy_server_runtime::{
    BoxFuture, DeserializableStruct, ErasedOperation, Operation, OperationContext,
    ServerHttpProtocol, Service,
};

use super::maintenance_type::addressed_type;

/// The body member the maintenance puts carry the configuration in, as a
/// violation path and the codec's own absence report name it.
const VALUE_MEMBER: &str = "value";

/// Whether the body of `req` carries no `value` member for the input of `O`.
///
/// Decided by the framework's own deserializer: `value` is `@required`, so a body
/// without it fails the read by naming that member — which is how this layer
/// cannot disagree with the walk about whether the request carried one. Every
/// other outcome is `false`: a body that reads, and one that fails for any other
/// reason (which [`super::request_layer`] is what answers).
fn value_is_absent<O: Operation>(protocol: &dyn ServerHttpProtocol, req: &RtRequest) -> bool {
    let Ok(mut deserializer) = protocol.deserialize_input(req, O::INPUT_SCHEMA) else {
        return false;
    };
    matches!(
        <O::Input as DeserializableStruct>::deserialize(&mut *deserializer),
        Err(SerdeError::MissingMember { member_name }) if member_name == VALUE_MEMBER
    )
}

/// The sentence this service states `req` with, when the operation `O` is one of
/// the maintenance puts and the request is one of the two classes this layer
/// answers. `resource_has_task` answers which types the addressed resource has.
fn refusal_for<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    resource_has_task: fn(&str) -> bool,
) -> Option<String> {
    // The PATH first: a request that addresses no configuration at all is
    // refused for the type whatever its body then carries.
    let addressed = addressed_type::<O>(req)?;
    if !resource_has_task(&addressed) {
        return Some(crate::state::invalid_maintenance_type_message(&addressed));
    }
    value_is_absent::<O>(protocol, req)
        .then(|| crate::state::MAINTENANCE_VALUE_MISSING_MESSAGE.to_string())
}

/// Dispatch the question on the routing key, one line per maintenance put,
/// naming the state enum of the tasks the resource it configures has.
///
/// The arm's key is the operation type's own name, because a REST protocol's
/// routing key IS the operation shape name: there is no second spelling of the
/// surface to drift from the first.
macro_rules! refusal_by_key {
    ($key:expr, $protocol:expr, $req:expr, $($module:ident :: $operation:ident => $tasks:ty),+ $(,)?) => {
        match $key {
            $(
                stringify!($operation) => refusal_for::<
                    crate::ops::$module::$operation<Eng>,
                >($protocol, $req, |wire| <$tasks>::from_wire(wire).is_some()),
            )+
            _ => None,
        }
    };
}

/// The sentence this service answers `req` with, when `req` resolved to a
/// maintenance put and is one of the two classes this layer states itself.
///
/// `None` covers every other outcome: a path naming one of the addressed
/// resource's tasks together with a body that carries a configuration value, and
/// every operation that is not a maintenance put.
fn refusal<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<String> {
    refusal_by_key!(
        key,
        protocol,
        req,
        put_table_maintenance_configuration::PutTableMaintenanceConfiguration
            => crate::state::TableMaintenanceType,
        put_table_bucket_maintenance_configuration::PutTableBucketMaintenanceConfiguration
            => crate::state::TableBucketMaintenanceType,
    )
}

/// Whether `req`, which the protocol resolved to the operation `key`, is one this
/// layer states a sentence for — the one question
/// [`crate::service::EmulatedService::override_operation`] asks of it.
pub(crate) fn refuses<Eng: Send + Sync + 'static>(
    key: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> bool {
    refusal::<Eng>(key, protocol, req).is_some()
}

/// The operation this service serves for a maintenance put it refuses in front of
/// dispatch.
///
/// An [`ErasedOperation`] rather than a response built in
/// [`Service::override_operation`], because that seam hands the router a BORROWED
/// operation. It holds no state: what the refusal states is read from the request
/// each time, so one value serves every such request.
///
/// `Eng` is the engine the service is generic over. It is never touched — the
/// refusal reads nothing and writes nothing — but naming it is what lets this
/// value sit on `EmulatedService<Eng>` beside the operations, which are generic
/// over the same engine, and what resolves the operation types the key is
/// dispatched on.
pub(crate) struct MaintenancePutRefusal<Eng> {
    engine: PhantomData<Eng>,
}

impl<Eng> MaintenancePutRefusal<Eng> {
    /// The one refusal value a service holds.
    pub(crate) const fn new() -> Self {
        Self {
            engine: PhantomData,
        }
    }
}

impl<Eng> Default for MaintenancePutRefusal<Eng> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for MaintenancePutRefusal<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // Rendered before the future, so no borrow of the service's definition is
        // held across an await: the refusal needs the request itself and nothing
        // else.
        let protocol = svc
            .override_protocol()
            .unwrap_or_else(|| svc.definition().protocol());
        let key = svc
            .definition()
            .protocol()
            .routing_key(req)
            .unwrap_or_default()
            .to_string();
        let response = match refusal::<Eng>(&key, protocol, req) {
            Some(sentence) => super::bad_request_response(svc, req, sentence),
            // The request resolved to this layer, so it is one of the classes
            // above; a request that is neither is never routed here.
            None => svc.definition().framework_error_response(
                ::aws_smithy_schema_server::ProtocolError::UnknownOperation(key),
                req,
            ),
        };
        Box::pin(async move { response })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", so
        // this layer cannot weaken the schemes the model declares for the
        // operation the request addressed.
        None
    }
}
