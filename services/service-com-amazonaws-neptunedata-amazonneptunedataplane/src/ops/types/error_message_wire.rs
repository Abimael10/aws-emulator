// SPDX-License-Identifier: CC0-1.0

//! The schema and the serializer every modelled neptunedata exception uses, because the
//! WIRE carries one member more than the on-disk model declares.
//!
//! WHY THIS MODULE EXISTS. Every exception this service declares carries the same three
//! modelled members — `detailedMessage`, `requestId` and `code` — and the data plane
//! writes the human-readable text under `message` as well, which is the member an SDK
//! client surfaces as the error's message. `#[smithy_macros::smithy(shape = ...)]` derives
//! a structure's `Schema` and its serializer from the model's member list and admits
//! EXACTLY that list, so a member the service really sends and the on-disk model omits
//! cannot be declared through the derive at all. [`neptune_error_wire`] emits, for one
//! exception, the same items the derive emits for an error shape — the `SCHEMA_<ERROR>`
//! static an `#[smithy_macros::operation_error]` enum arm names, `shape_name`, `Display`,
//! `std::error::Error` and the serializer — from a member list this file states, with the
//! unmodelled member appended LAST so a later model refresh that declares it only has to
//! move it into place and put the derive back.
//!
//! `message` is not a field of its own: a refusal carries ONE text, so the serializer
//! writes `detailedMessage`'s value under both names rather than letting two copies of one
//! sentence drift apart. `code` is not a field either: the code a client branches on is the
//! error CLASS, which is the exception shape's own name, so the serializer writes
//! `shape_name()` there and no payload builder can state a status number or a different
//! class in it.
//!
//! WHAT IS DELIBERATELY ABSENT, because the derive's own output would be inert here too:
//! constraint traits (the validating walk runs on operation INPUTS, and an exception is
//! output-only), the `@http` chain (baked on only for an operation's input), `@xmlName`
//! (this service speaks restJson1), and a deserializer (a refusal this service answers is
//! only ever written).
//!
//! The shapes that use it live in this service's `types/` modules, so `types/mod.rs` pulls
//! this module in with `#[macro_use]` ahead of them — which is what puts the macros in
//! their textual scope. They are NOT `#[macro_export]`ed: `src/ops/mod.rs` reaches `types`
//! through an `include!`, so a `macro_rules!` declared under it counts as macro-expanded
//! and cannot be named by an absolute `$crate::` path (rust-lang #52234).

/// One member's schema, as the vendored codegen emits it: the member's shape id, its
/// shape type, its wire name and its index in the member list.
macro_rules! neptune_member_schema {
    ($ns:literal, $local:literal, $wire:literal, $idx:literal) => {
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                concat!($ns, "#", $local, "$", $wire),
                $ns,
                $wire,
            ),
            ::aws_smithy_schema::ShapeType::String,
            $wire,
            $idx,
        )
    };
}

/// The schemas, the wire name, the prose and the serializer for one exception shape: the
/// three members the model declares, plus the `message` the data plane writes the same
/// text under.
macro_rules! neptune_error_wire {
    ($ty:ident, $schema_static:ident, $shape:literal, $ns:literal, $local:literal $(,)?) => {
        impl $ty {
            /// `detailedMessage`: "A detailed message describing the problem."
            pub const M_DETAILED_MESSAGE: ::aws_smithy_schema::Schema =
                neptune_member_schema!($ns, $local, "detailedMessage", 0);
            /// `requestId`: the id of the request this refusal answers.
            pub const M_REQUEST_ID: ::aws_smithy_schema::Schema =
                neptune_member_schema!($ns, $local, "requestId", 1);
            /// `code`: the wire error code this refusal carries — the exception's own NAME,
            /// which is what a client branches on. The on-disk model documents the member as
            /// the HTTP status code, and the data plane answers the exception name there:
            /// the status is already on the response's status line, so the member a client
            /// reads for the error CLASS carries the class.
            pub const M_CODE: ::aws_smithy_schema::Schema =
                neptune_member_schema!($ns, $local, "code", 2);
            /// `message`: the member the on-disk model omits and the data plane writes the
            /// refusal's text under — the one an SDK client reads as the error message.
            pub const M_MESSAGE: ::aws_smithy_schema::Schema =
                neptune_member_schema!($ns, $local, "message", 3);

            pub const SCHEMA: ::aws_smithy_schema::Schema =
                ::aws_smithy_schema::Schema::new_struct(
                    ::aws_smithy_schema::ShapeId::from_static($shape, $ns, $local),
                    ::aws_smithy_schema::ShapeType::Structure,
                    &[
                        &Self::M_DETAILED_MESSAGE,
                        &Self::M_REQUEST_ID,
                        &Self::M_CODE,
                        &Self::M_MESSAGE,
                    ],
                );

            /// The wire error code this refusal is matched on: the error shape's own name.
            pub const fn shape_name() -> &'static str {
                $local
            }
        }

        pub static $schema_static: ::aws_smithy_schema::Schema = $ty::SCHEMA;

        impl ::std::fmt::Display for $ty {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}: {}", $local, self.detailed_message)
            }
        }

        impl ::std::error::Error for $ty {}

        impl ::aws_smithy_schema::serde::SerializableStruct for $ty {
            fn serialize_members(
                &self,
                s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
            ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
                s.write_string(&Self::M_DETAILED_MESSAGE, self.detailed_message.as_str())?;
                s.write_string(&Self::M_REQUEST_ID, self.request_id.as_str())?;
                // The exception's NAME, not the HTTP status: the code a client branches on
                // is the error class, and it is the shape's own name wherever this
                // serializer is used, so no payload builder can state a different one.
                s.write_string(&Self::M_CODE, Self::shape_name())?;
                // ONE text, written under both names: `message` is where a client reads
                // the refusal, `detailedMessage` is where the model declares it.
                s.write_string(&Self::M_MESSAGE, self.detailed_message.as_str())?;
                ::std::result::Result::Ok(())
            }
        }
    };
}

/// One exception shape of this service: the structure, and the wire items
/// [`neptune_error_wire`] emits for it.
///
/// Every exception `com.amazonaws.neptunedata` declares carries the same three members,
/// so one invocation per shape states the whole shape: its Rust type, the `SCHEMA_<ERROR>`
/// static an `#[smithy_macros::operation_error]` arm names, and the shape id, namespace
/// and local name the wire carries.
macro_rules! neptune_error_shape {
    ($ty:ident, $schema_static:ident, $shape:literal, $ns:literal, $local:literal $(,)?) => {
        #[doc = concat!(
            "The modelled `", $local, "` refusal. Carries the two members whose value is \
             this refusal's own — `detailedMessage` and `requestId` — and is written to the \
             wire with `message` and `code` as well; see `types::error_message_wire`."
        )]
        #[derive(Debug, Clone, Default)]
        pub struct $ty {
            pub detailed_message: String,
            pub request_id: String,
        }

        neptune_error_wire!($ty, $schema_static, $shape, $ns, $local);
    };
}
