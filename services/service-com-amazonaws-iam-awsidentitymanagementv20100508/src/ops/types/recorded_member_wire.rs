// SPDX-License-Identifier: CC0-1.0

//! The schema and the codec one structure needs when the WIRE carries a member the on-disk
//! model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema` and its codec from the model's member list, and it admits EXACTLY that list: a
//! Rust field with no member behind it is a compile error, so a member the service really
//! reports but the on-disk model omits cannot be declared through the derive at all. The
//! macros below emit, for one structure, the same items the derive emits — one `M_<MEMBER>`
//! member schema per member (index = that member's position in the member list), a `SCHEMA`
//! naming them in that order, the `<STRUCT>_SCHEMA` static and the `SerializableStruct` /
//! `DeserializableStruct` pair — from a member list given here, so a member the model does
//! not declare can be appended to it.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="<Shape>")` lists them in), and a member the model does
//! not declare is appended LAST with its shape id in a namespace of its own, so it can
//! neither claim to be modelled nor collide with a member the model may declare later — and a
//! later model refresh only has to move it into place and put the derive back.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and no
//! member below carries a bound of its own), the `@http` chain (baked on only for an
//! operation's input), and `@xmlName` with the XML list layout (no member or list below
//! declares either: each member's wire name IS its Smithy member name and each list carries
//! the default `member` item element).
//!
//! The structures that need them live in this service's per-operation `types/` modules, where
//! each shape is declared exactly once, so `types/mod.rs` pulls this module in with
//! `#[macro_use]` ahead of those modules, which is what puts these macros in their textual
//! scope. They are NOT `#[macro_export]`ed: `src/ops/mod.rs` reaches `types` through an
//! `include!`, so a `macro_rules!` declared under it counts as macro-expanded and cannot be
//! named by an absolute `$crate::` path (rust-lang #52234) — which is also why a service that
//! needs the same machinery cannot import this module and carries its own copy.
//!
//! REMOVE THIS MODULE when the vendored codegen learns to declare a member the on-disk model
//! omits. That gap is in `vendor/smithy-macros` — `#[smithy_macros::smithy(shape = ...)]`
//! admits exactly the model's member list — and it is not this service's alone: the same
//! machinery is hand-written for the same reason in
//! `services/service-com-amazonaws-athena-amazonathena/src/ops/types/recorded_member_wire.rs`,
//! `services/service-com-amazonaws-s3control-awss3controlservicev20180820/src/ops/types/
//! unmodelled_member_wire.rs` and
//! `services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/types/
//! unmodelled_member_wire.rs`. Once the generator takes a recorded member, delete these
//! macros and the `iam_schema_and_codec!` blocks and put the derive back on the
//! structures that carry one; `tests/all/saml_provider_metadata.rs` pins the wire form either
//! way.

/// The namespace an unmodelled member's and an unmodelled shape's own id sits in: not this
/// service's, so nothing built here can claim to be modelled or collide with a shape the
/// model may declare later.
///
/// Documentation, not the source of the literal: `iam_member_schema!` builds a shape id with
/// `concat!`, which takes a string literal and not a `const`, so the namespace is written out
/// there and in each `iam_schema_and_codec!` block that declares a recorded shape.
/// Change it here and in those places together.
pub const RECORDED_NAMESPACE: &str = "com.amazonaws.iam.recorded";

/// The `ShapeType` one member kind names. Mirrors the vendored codegen's
/// `codegen::shape_type_ident`.
macro_rules! iam_shape_type {
    (string) => {
        ::aws_smithy_schema::ShapeType::String
    };
    (boolean) => {
        ::aws_smithy_schema::ShapeType::Boolean
    };
    (date_time) => {
        ::aws_smithy_schema::ShapeType::Timestamp
    };
    (list_of_struct) => {
        ::aws_smithy_schema::ShapeType::List
    };
    (list_of_string) => {
        ::aws_smithy_schema::ShapeType::List
    };
}

/// One member's schema, as the vendored codegen emits it: the member's shape id, its shape
/// type, its wire name and its index in the member list.
///
/// `$kind` is the member's shape kind — `string`, `boolean`, `date_time`,
/// `list_of_struct(T)`, `list_of_string` — or `recorded(<kind>)` for a member the model does
/// not declare at all, whose shape id then sits in [`RECORDED_NAMESPACE`] rather than in the
/// service's own.
macro_rules! iam_member_schema {
    ($ns:literal, $local:literal, $wire:literal, $idx:literal, recorded($kind:tt $(($inner:ty))?)) => {
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                concat!("com.amazonaws.iam.recorded", "#", $local, "$", $wire),
                "com.amazonaws.iam.recorded",
                $wire,
            ),
            iam_shape_type!($kind),
            $wire,
            $idx,
        )
    };
    ($ns:literal, $local:literal, $wire:literal, $idx:literal, $kind:tt $(($inner:ty))?) => {
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                concat!($ns, "#", $local, "$", $wire),
                $ns,
                $wire,
            ),
            iam_shape_type!($kind),
            $wire,
            $idx,
        )
    };
}

/// Write one member, as the vendored codegen's `codegen::emit_write` does for that kind.
macro_rules! iam_write_member {
    // A member the model does not declare writes exactly like the kind it wraps; only its
    // shape id differs (see `iam_member_schema!`).
    ($s:ident, $m:expr, $v:expr, recorded($($kind:tt)+)) => {
        iam_write_member!($s, $m, $v, $($kind)+);
    };
    ($s:ident, $m:expr, $v:expr, string) => {
        $s.write_string($m, $v.as_str())?;
    };
    ($s:ident, $m:expr, $v:expr, boolean) => {
        $s.write_boolean($m, *$v)?;
    };
    ($s:ident, $m:expr, $v:expr, date_time) => {
        $s.write_timestamp($m, $v)?;
    };
    // A list writes its elements through the ELEMENT's own schema, exactly as the vendored
    // codegen's list branch does: the member schema is what names the member on the wire, and
    // the element schema is what each element is written against.
    ($s:ident, $m:expr, $v:expr, list_of_struct($inner:ty)) => {
        $s.write_list($m, &|__sl| {
            for __it in $v.iter() {
                __sl.write_struct(&<$inner>::SCHEMA, __it)?;
            }
            ::std::result::Result::Ok(())
        })?;
    };
    ($s:ident, $m:expr, $v:expr, list_of_string) => {
        $s.write_list($m, &|__sl| {
            for __it in $v.iter() {
                __sl.write_string($m, __it.as_str())?;
            }
            ::std::result::Result::Ok(())
        })?;
    };
}

/// Read one member, as the vendored codegen's `codegen::emit_read` does for that kind.
///
/// Nesting DEPTH is bounded by the codec itself, which refuses a document nesting deeper
/// than its own maximum.
macro_rules! iam_read_member {
    ($d:ident, $m:expr, recorded($($kind:tt)+)) => {
        iam_read_member!($d, $m, $($kind)+)
    };
    ($d:ident, $m:expr, string) => {
        $d.read_string($m)?
    };
    ($d:ident, $m:expr, boolean) => {
        $d.read_boolean($m)?
    };
    ($d:ident, $m:expr, date_time) => {
        $d.read_timestamp($m)?
    };
    ($d:ident, $m:expr, list_of_struct($inner:ty)) => {{
        let mut __items: ::std::vec::Vec<$inner> = ::std::vec::Vec::new();
        $d.read_list($m, &mut |__d| {
            __items.push(
                <$inner as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(__d)?,
            );
            ::std::result::Result::Ok(())
        })?;
        __items
    }};
    ($d:ident, $m:expr, list_of_string) => {{
        let mut __items: ::std::vec::Vec<::std::string::String> = ::std::vec::Vec::new();
        $d.read_list($m, &mut |__d| {
            __items.push(__d.read_string($m)?);
            ::std::result::Result::Ok(())
        })?;
        __items
    }};
}

/// The schema and the codec for one structure, from the member list given here:
/// everything `#[smithy_macros::smithy(shape = ...)]` emits for a structure, for a
/// member list that may carry a member the on-disk model does not declare.
///
/// Each member entry is `(CONST, field, "WireName", index, kind)`; every field is an
/// `Option`, which is what every member of every structure below is, so a member is written
/// exactly when the value is there. The entries must be given in the model's member order —
/// the order `smithy_query(query="shape", shape=...)` lists them in and the order the struct
/// declares its fields in — with an unmodelled member last.
macro_rules! iam_schema_and_codec {
    (
        $ty:ident, $schema_static:ident, $shape:literal, $ns:literal, $local:literal,
        $( ( $cname:ident, $field:ident, $wire:literal, $idx:literal, $($kind:tt)+ ) ),+ $(,)?
    ) => {
        #[allow(non_upper_case_globals)]
        impl $ty {
            $(
                pub const $cname: ::aws_smithy_schema::Schema =
                    iam_member_schema!($ns, $local, $wire, $idx, $($kind)+);
            )+

            pub const SCHEMA: ::aws_smithy_schema::Schema =
                ::aws_smithy_schema::Schema::new_struct(
                    ::aws_smithy_schema::ShapeId::from_static($shape, $ns, $local),
                    ::aws_smithy_schema::ShapeType::Structure,
                    &[$(&Self::$cname,)+],
                );
        }

        pub static $schema_static: &::aws_smithy_schema::Schema = &$ty::SCHEMA;

        impl ::aws_smithy_schema::serde::SerializableStruct for $ty {
            fn serialize_members(
                &self,
                s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
            ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
                $(
                    if let ::std::option::Option::Some(__v) = self.$field.as_ref() {
                        iam_write_member!(s, &Self::$cname, __v, $($kind)+);
                    }
                )+
                Ok(())
            }
        }

        impl ::aws_smithy_schema_server::DeserializableStruct for $ty {
            fn deserialize(
                d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
            ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
                $(
                    let mut $field = ::std::option::Option::None;
                )+
                d.read_struct(&Self::SCHEMA, &mut |member, d| {
                    match member.member_index() {
                        $(
                            ::std::option::Option::Some($idx) => {
                                $field = ::std::option::Option::Some(
                                    iam_read_member!(d, &Self::$cname, $($kind)+)
                                );
                            }
                        )+
                        _ => {}
                    }
                    Ok(())
                })?;
                Ok(Self { $( $field ),+ })
            }
        }
    };
}
