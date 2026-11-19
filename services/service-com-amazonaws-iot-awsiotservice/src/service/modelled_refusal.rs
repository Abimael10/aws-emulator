// SPDX-License-Identifier: CC0-1.0

//! The error SHAPE a constraint refusal of this service names, for the operations whose
//! model declares another one than the classic surface's — and the COLLECTION such a
//! refusal quotes.
//!
//! # The rule
//!
//! A request outside the constraints its model declares is refused before any handler runs,
//! with the error shape the OPERATION models for an invalid request. AWS IoT has two such
//! shapes: the classic surface's legacy `InvalidRequestException`, which
//! [`super::validation`] states, and `ValidationException` — modelled by the operations of
//! the software-package catalog and the commands plane
//! (`smithy_query(query="operation",operation="CreateCommand")` lists it among
//! `CreateCommand`'s errors, as every operation named in [`stated_refusals`] does) — which
//! is what those operations answer. A refusal naming a shape the operation does not model
//! is one a client cannot branch on: it reads the shape name and finds no such error in the
//! API it called.
//!
//! The clause is the same sentence either way, and so is the half of it that states the
//! rejected value: where that value is a COLLECTION, AWS quotes the collection the request
//! submitted — a list as `[<element>, …]`, a map as `{<key>=<value>, …}` — while the
//! framework's walk, which length-checks a collection member before it descends into the
//! entries, has only a summary of its SIZE and so quotes a value no caller sent.
//!
//! # Why a per-operation layer
//!
//! [`Service::override_refusal`](smithy_server_runtime::Service::override_refusal) is handed
//! the violations alone — neither the operation they were found in nor the request they came
//! from — so neither fact can be recovered there. This layer is where both are:
//! [`StatingModelledRefusal`] wraps ONE operation, so the shape is that operation's, and it
//! holds the request, so the submitted collection can be read back out of it.
//!
//! Nothing is re-implemented and no check is repeated in other words: the violations come
//! from this tree's shared collecting walk
//! ([`::service_common::validation::refusing_violations_with_mapped_echoes`], which runs the
//! protocol's own deserializer and the framework's own constraint walk and owns every bound
//! on what one clause may quote), the restoring rule is
//! [`::service_common::validation_message::quoting_the_submitted_collection`], the envelope
//! and the clauses are [`super::validation`]'s — the same ones every other operation of this
//! service is refused with — and the wire rendering is the framework's own refusal channel.
//! A request this layer does not refuse is handed to the wrapped operation untouched, which
//! still owns the whole deserialize -> validate -> handle -> serialize sequence.

use std::collections::HashSet;
use std::sync::Arc;

use ::aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse};
use ::smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, OperationRoutingTable, Refusal,
    Service, Violation,
};

/// The error shape the software-package and commands planes of this service model for an
/// invalid request, named bare on the wire: it is declared in this service's own namespace,
/// and only the two legacy shapes carry the API doc namespace
/// ([`super::NAMESPACED_ERROR_SHAPES`]).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// One modelled operation, served so that a constraint refusal of it names the error shape
/// the operation MODELS and quotes the collection the request submitted.
///
/// It makes no routing decision and holds no state of its own: the operation it wraps is the
/// one the generated routing table names for the same key, built from the SAME shared
/// engine, so what a request that satisfies the model is answered is unchanged.
pub struct StatingModelledRefusal<Op> {
    operation: Op,
    /// The error shape this operation's model declares for an invalid request.
    code: &'static str,
}

impl<Op> StatingModelledRefusal<Op> {
    /// Wrap one operation, already constructed from the shared engine.
    fn new(operation: Op, code: &'static str) -> Self {
        Self { operation, code }
    }
}

impl<Op> ErasedOperation for StatingModelledRefusal<Op>
where
    Op: Operation + ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r WireRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, WireResponse> {
        Box::pin(async move {
            // The whole refusal is decided — and the borrow of the definition it is
            // rendered through taken and dropped — BEFORE the await below:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding one across that
            // await would make this future non-`Send`.
            if let Some(refused) = stated_refusal::<Op>(svc, req, self.code) {
                return refused;
            }
            self.operation.run(svc, req, ctx).await
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not
    /// change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

/// The refusal `req` earns from `Op`'s own modelled constraints, worded as this service
/// words one and under the shape `code`, or `None` for a request that breaks none of them —
/// which is every request the wrapped operation then serves.
fn stated_refusal<Op: Operation>(
    svc: &dyn Service,
    req: &WireRequest,
    code: &str,
) -> Option<WireResponse> {
    let (refusing, _lists) =
        ::service_common::validation::refusing_violations_with_mapped_echoes::<Op>(
            svc.definition().protocol(),
            req,
        );
    if refusing.is_empty() {
        return None;
    }
    // A clause about a COLLECTION member quotes the collection the request carried, where
    // the framework's walk stored its own summary of that collection's size. Which member
    // paths that happened at travels with the violations, so the statement below states
    // those clauses WITH their value and every other length clause without one.
    let mut quoting_a_collection: HashSet<String> = HashSet::new();
    let violations: Vec<Violation> = refusing
        .into_iter()
        .map(|(violation, echo)| {
            let path = violation.path.clone();
            let summarised = violation.value.clone();
            let quoted =
                ::service_common::validation_message::quoting_the_submitted_collection(
                    violation, echo,
                );
            if quoted.value != summarised {
                quoting_a_collection.insert(path);
            }
            quoted
        })
        .collect();
    let stated = super::validation::refused_under(
        &Refusal::Invalid(violations.clone()),
        code,
        &quoting_a_collection,
    )?;
    Some(svc.definition().refusal_response(
        ::aws_smithy_schema_server::ProtocolError::Invalid(violations),
        Some(stated),
        req,
    ))
}

/// Serve `$operation` through this layer, each built from the shared engine: one
/// `crate::ops::<module>::<Operation>` per line, filed under its own modelled name.
macro_rules! serve {
    ($table:expr, $engine:expr, $code:expr, $($module:ident :: $operation:ident),+ $(,)?) => {
        $(
            $table.add_operation(
                <crate::ops::$module::$operation<Eng> as Operation>::NAME,
                ::std::boxed::Box::new(StatingModelledRefusal::new(
                    crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                    $code,
                )),
            );
        )+
    };
}

/// The operations of this service whose model declares [`VALIDATION_EXCEPTION`] for an
/// invalid request, each wrapped so that a constraint refusal of it names that shape, and
/// built from the SAME shared engine, so a wrapped operation reads and writes the one store.
///
/// The list is the model's: these are the operations of the software-package catalog and the
/// commands plane, the two planes whose errors include that shape
/// (`smithy_query(query="operation",operation="<name>")` for any of them). Every OTHER
/// operation of this service is served by the generated routing table and refused by
/// [`super::validation`] under the classic surface's legacy shape, so no operation has to be
/// named here to be refused correctly.
pub(super) fn stated_refusals<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
) -> OperationRoutingTable {
    let mut table = OperationRoutingTable::new();
    serve!(
        table,
        engine,
        VALIDATION_EXCEPTION,
        associate_sbom_with_package_version::AssociateSbomWithPackageVersion,
        create_command::CreateCommand,
        create_package::CreatePackage,
        create_package_version::CreatePackageVersion,
        delete_command::DeleteCommand,
        delete_command_execution::DeleteCommandExecution,
        delete_package::DeletePackage,
        delete_package_version::DeletePackageVersion,
        disassociate_sbom_from_package_version::DisassociateSbomFromPackageVersion,
        get_command::GetCommand,
        get_command_execution::GetCommandExecution,
        get_package::GetPackage,
        get_package_version::GetPackageVersion,
        list_command_executions::ListCommandExecutions,
        list_commands::ListCommands,
        list_package_versions::ListPackageVersions,
        list_packages::ListPackages,
        list_sbom_validation_results::ListSbomValidationResults,
        update_command::UpdateCommand,
        update_package::UpdatePackage,
        update_package_configuration::UpdatePackageConfiguration,
        update_package_version::UpdatePackageVersion,
    );
    table
}
