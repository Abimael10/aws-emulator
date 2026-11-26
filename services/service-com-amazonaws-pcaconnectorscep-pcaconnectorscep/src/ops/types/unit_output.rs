// SPDX-License-Identifier: CC0-1.0

//! The ONE spelling of the output of an operation modelled with output `smithy.api#Unit`.
//!
//! Four of this service's operations are modelled that way — `DeleteChallenge`,
//! `DeleteConnector`, `TagResource` and `UntagResource` — so their whole answer is a status
//! with no body, and the only thing that differs between them is the `@http` binding the
//! model gives each: the method, the URI and the response status. Everything else (the
//! member-less output struct, the `Unit` schema it carries, the static the generated
//! `impl Operation` names as `OUTPUT_SCHEMA`, and the member serializer that writes nothing)
//! is one rule, so it is written once here and expanded per operation by [`unit_output!`].
//!
//! The macro is re-exported by `types`, so an operation module invokes it as
//! `super::types::unit_output!(...)` and the items it declares land in that operation's own
//! module, exactly where a hand-written copy used to sit.

/// Declare the output of an operation whose modelled output shape is `smithy.api#Unit`.
///
/// Expands, in the invoking operation module, to:
///
/// * `pub struct $output;` — the member-less output type the handler returns;
/// * `$output::SCHEMA` — the `smithy.api#Unit` structure schema, carrying the operation's own
///   `@http` binding. `with_http` is what carries the modelled response status: without it the
///   status falls back to the protocol default of 200, whatever status the model declares, so
///   `status` here is the one the model gives this operation (and the one the handler's
///   `SUCCESS_STATUS` repeats);
/// * `pub static $schema_static` — the schema reference the generated `impl Operation` names
///   as its `OUTPUT_SCHEMA`;
/// * a `SerializableStruct` impl that serializes no member, because `Unit` has none.
///
/// `method`, `uri` and `status` are the three values of this operation's `@http` trait; ask
/// the model for them (`smithy_query(query="operation", operation="<Op>")`) rather than
/// copying them from another operation.
macro_rules! unit_output {
    (
        $output:ident,
        $schema_static:ident,
        method = $method:expr,
        uri = $uri:expr,
        status = $status:expr
        $(,)?
    ) => {
        /// This operation's output: the modelled `smithy.api#Unit`, which carries no member,
        /// so the response body is empty and the status is the whole answer.
        #[derive(Debug, Clone, Default)]
        pub struct $output;

        impl $output {
            /// The `smithy.api#Unit` output schema, carrying this operation's `@http`
            /// binding — the method, the URI and the modelled response status.
            pub const SCHEMA: ::aws_smithy_schema::Schema =
                ::aws_smithy_schema::Schema::new_struct(
                    ::aws_smithy_schema::ShapeId::from_static(
                        "smithy.api#Unit",
                        "smithy.api",
                        "Unit",
                    ),
                    ::aws_smithy_schema::ShapeType::Structure,
                    &[],
                )
                .with_http(::aws_smithy_schema::traits::HttpTrait::new(
                    $method,
                    $uri,
                    ::std::option::Option::Some($status),
                ));
        }

        /// The schema the generated `impl Operation` names as its `OUTPUT_SCHEMA`.
        pub static $schema_static: &::aws_smithy_schema::Schema = &$output::SCHEMA;

        impl ::aws_smithy_schema::serde::SerializableStruct for $output {
            fn serialize_members(
                &self,
                _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
            ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
                ::std::result::Result::Ok(())
            }
        }
    };
}

pub(crate) use unit_output;
