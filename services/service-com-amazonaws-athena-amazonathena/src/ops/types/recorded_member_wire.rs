// SPDX-License-Identifier: CC0-1.0

//! The schema and the codec one structure needs when the WIRE carries a member the on-disk
//! model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the model's member list, and it admits
//! EXACTLY that list: a Rust field with no member behind it is a compile error, so a member
//! the service really reports but the on-disk model omits cannot be declared through the
//! derive at all. The macros below emit, for one structure, the same items the derive emits —
//! one `M_<MEMBER>` member schema per member (index = that member's position in the member
//! list), a `SCHEMA` naming them in that order, the `<STRUCT>_SCHEMA` static and the
//! `SerializableStruct` / `DeserializableStruct` pair — from a member list given here, so a
//! member the model does not declare can be appended to it.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="<Shape>")` lists them in), and a member the model does
//! not declare is appended LAST with its shape id in a namespace of its own, so it can
//! neither claim to be modelled nor collide with a member the model may declare later — and a
//! later model refresh only has to move it into place and put the derive back.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the `@http` chain (baked on only for an operation's input) and `@jsonName` (no member
//! below declares one: each one's wire name IS its Smithy member name). The constraint traits
//! are NOT absent — a shape declared here may also be an operation INPUT, whose values the
//! framework's validating walk measures against the trait map on each member schema — so a
//! member carrying a bound of its own names its trait map through `traits(<static>)`; see
//! `ath_member_schema!`.
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
//! `services/service-com-amazonaws-s3control-awss3controlservicev20180820/src/ops/types/
//! unmodelled_member_wire.rs` and
//! `services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/types/
//! unmodelled_member_wire.rs`, alongside the `date_time_wire.rs` modules that compensate for
//! the neighbouring `@timestampFormat` gap. Once the generator takes a recorded member, delete
//! these macros and the `ath_schema_and_codec!` blocks and put the derive back on the
//! structures that carry one; `tests/all/query_execution_detail.rs` pins the wire form either
//! way.

/// The namespace an unmodelled member's and an unmodelled shape's own id sits in: not this
/// service's, so nothing built here can claim to be modelled or collide with a shape the
/// model may declare later.
///
/// Documentation, not the source of the literal: `ath_member_schema!` builds a shape id with
/// `concat!`, which takes a string literal and not a `const`, so the namespace is written out
/// there and in each `ath_schema_and_codec!` block that declares a recorded shape. Change it
/// here and in those three places together.
pub const RECORDED_NAMESPACE: &str = "com.amazonaws.athena.recorded";

/// The `ShapeType` one member kind names. Mirrors the vendored codegen's
/// `codegen::shape_type_ident`.
macro_rules! ath_shape_type {
    (string) => {
        ::aws_smithy_schema::ShapeType::String
    };
    (boolean) => {
        ::aws_smithy_schema::ShapeType::Boolean
    };
    (integer) => {
        ::aws_smithy_schema::ShapeType::Integer
    };
    (long) => {
        ::aws_smithy_schema::ShapeType::Long
    };
    (date_time) => {
        ::aws_smithy_schema::ShapeType::Timestamp
    };
    (struct_of) => {
        ::aws_smithy_schema::ShapeType::Structure
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
/// `$kind` is the member's shape kind — `string`, `boolean`, `integer`, `long`, `date_time`,
/// `struct_of(T)`, `list_of_struct(T)`, `list_of_string` — or `recorded(<kind>)` for a member
/// the model does not declare at all,
/// whose shape id then sits in [`RECORDED_NAMESPACE`] rather than in the service's own.
///
/// A member whose own shape carries a CONSTRAINT trait — `@length`, `@range`, `@pattern` —
/// names the [`::aws_smithy_schema::TraitMap`] holding them first, as `traits(<static>),`
/// ahead of the kind. That is the one thing the derive emits that a bare member schema would
/// drop: the framework's validating walk enforces an input constraint out of the member
/// schema's trait map, so a hand-written member that carries one must carry its traits too or
/// a value the model refuses would be accepted. The map is a `static LazyLock<TraitMap>` at
/// module scope — it cannot live in the `impl` block these member schemas are declared in —
/// built exactly as the vendored codegen builds its own.
macro_rules! ath_member_schema {
    (
        $ns:literal, $local:literal, $wire:literal, $idx:literal,
        traits($traits:path), $($kind:tt)+
    ) => {
        ath_member_schema!($ns, $local, $wire, $idx, $($kind)+).with_traits(&$traits)
    };
    ($ns:literal, $local:literal, $wire:literal, $idx:literal, recorded($kind:tt $(($inner:ty))?)) => {
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                concat!("com.amazonaws.athena.recorded", "#", $local, "$", $wire),
                "com.amazonaws.athena.recorded",
                $wire,
            ),
            ath_shape_type!($kind),
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
            ath_shape_type!($kind),
            $wire,
            $idx,
        )
    };
}

/// Write one member, as the vendored codegen's `codegen::emit_write` does for that kind.
macro_rules! ath_write_member {
    // The member's constraint traits are read by the VALIDATING walk off its schema; what is
    // written, and how, is decided by the kind alone.
    ($s:ident, $m:expr, $v:expr, traits($traits:path), $($kind:tt)+) => {
        ath_write_member!($s, $m, $v, $($kind)+);
    };
    // A member the model does not declare writes exactly like the kind it wraps; only its
    // shape id differs (see `ath_member_schema!`).
    ($s:ident, $m:expr, $v:expr, recorded($($kind:tt)+)) => {
        ath_write_member!($s, $m, $v, $($kind)+);
    };
    ($s:ident, $m:expr, $v:expr, string) => {
        $s.write_string($m, $v.as_str())?;
    };
    ($s:ident, $m:expr, $v:expr, boolean) => {
        $s.write_boolean($m, *$v)?;
    };
    ($s:ident, $m:expr, $v:expr, integer) => {
        $s.write_integer($m, *$v)?;
    };
    ($s:ident, $m:expr, $v:expr, long) => {
        $s.write_long($m, *$v)?;
    };
    ($s:ident, $m:expr, $v:expr, date_time) => {
        $s.write_timestamp($m, $v)?;
    };
    ($s:ident, $m:expr, $v:expr, struct_of($inner:ty)) => {
        $s.write_struct($m, $v)?;
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
macro_rules! ath_read_member {
    ($d:ident, $m:expr, traits($traits:path), $($kind:tt)+) => {
        ath_read_member!($d, $m, $($kind)+)
    };
    ($d:ident, $m:expr, recorded($($kind:tt)+)) => {
        ath_read_member!($d, $m, $($kind)+)
    };
    ($d:ident, $m:expr, string) => {
        $d.read_string($m)?
    };
    ($d:ident, $m:expr, boolean) => {
        $d.read_boolean($m)?
    };
    ($d:ident, $m:expr, integer) => {
        $d.read_integer($m)?
    };
    ($d:ident, $m:expr, long) => {
        $d.read_long($m)?
    };
    ($d:ident, $m:expr, date_time) => {
        $d.read_timestamp($m)?
    };
    ($d:ident, $m:expr, struct_of($inner:ty)) => {
        <$inner as ::aws_smithy_schema_server::DeserializableStruct>::deserialize($d)?
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

/// Write a member only when the value is there: an `Option` one when it is `Some`, a
/// non-`Option` one (`@required`) always — the vendored codegen's own `is_option` split. An
/// `out` member is an `Option` one the service only ever REPORTS, so it writes like `opt`.
macro_rules! ath_write_present_member {
    ($s:ident, $this:ident, $m:path, $field:ident, req, $($kind:tt)+) => {
        ath_write_member!($s, &$m, (&$this.$field), $($kind)+);
    };
    ($s:ident, $this:ident, $m:path, $field:ident, $presence:tt, $($kind:tt)+) => {
        if let ::std::option::Option::Some(__v) = $this.$field.as_ref() {
            ath_write_member!($s, &$m, __v, $($kind)+);
        }
    };
}

/// Fill one read slot from the wire — except for an `out` member, whose value is read and
/// DISCARDED: the service computes that member and reports it, so nothing a request carries
/// can set it, and the value still has to be consumed to keep the codec in step with the
/// document.
macro_rules! ath_read_present_member {
    ($d:ident, $slot:ident, $m:expr, out, $($kind:tt)+) => {
        let _ = ath_read_member!($d, $m, $($kind)+);
    };
    ($d:ident, $slot:ident, $m:expr, $presence:tt, $($kind:tt)+) => {
        $slot = ::std::option::Option::Some(ath_read_member!($d, $m, $($kind)+));
    };
}

/// The value a read slot contributes to the struct: a `@required` member absent from the wire
/// is refused by name, an optional one stays `None`.
macro_rules! ath_member_value {
    ($slot:ident, $wire:literal, req) => {
        $slot.ok_or_else(|| ::aws_smithy_schema_server::missing_member($wire))?
    };
    ($slot:ident, $wire:literal, $presence:tt) => {
        $slot
    };
}

/// The schema, the serializer and the deserializer for one structure, from the member list
/// given here: everything `#[smithy_macros::smithy(shape = ...)]` emits for a structure, for
/// a member list that may carry a member the on-disk model does not declare.
///
/// Each member entry is `(CONST, field, "WireName", index, req|opt|out, kind)`; `req` is a
/// `@required` member (a bare Rust field, refused when absent on the wire), `opt` an `Option`
/// one, and `out` an `Option` one the service only REPORTS — never read off the wire, so a
/// request cannot set what the service itself computes. A member whose shape carries a
/// constraint trait names its trait map ahead of the kind, as `traits(<static>), kind`. The
/// entries must be given in the model's member order — the order
/// `smithy_query(query="shape", shape=...)` lists them in and the order the struct declares
/// its fields in — with an unmodelled member last.
macro_rules! ath_schema_and_codec {
    (
        $ty:ident, $schema_static:ident, $shape:literal, $ns:literal, $local:literal,
        $( ( $cname:ident, $field:ident, $wire:literal, $idx:literal, $presence:tt, $($kind:tt)+ ) ),+ $(,)?
    ) => {
        #[allow(non_upper_case_globals)]
        impl $ty {
            $(
                pub const $cname: ::aws_smithy_schema::Schema =
                    ath_member_schema!($ns, $local, $wire, $idx, $($kind)+);
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
                    ath_write_present_member!(
                        s, self, Self::$cname, $field, $presence, $($kind)+
                    );
                )+
                Ok(())
            }
        }

        impl ::aws_smithy_schema_server::DeserializableStruct for $ty {
            fn deserialize(
                d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
            ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
                // `allow(unused_mut)`: an `out` member's slot is never filled from the
                // wire, so nothing assigns it.
                $(
                    #[allow(unused_mut)]
                    let mut $field = ::std::option::Option::None;
                )+
                d.read_struct(&Self::SCHEMA, &mut |member, d| {
                    match member.member_index() {
                        $(
                            ::std::option::Option::Some($idx) => {
                                ath_read_present_member!(
                                    d, $field, &Self::$cname, $presence, $($kind)+
                                );
                            }
                        )+
                        _ => {}
                    }
                    Ok(())
                })?;
                Ok(Self {
                    $( $field: ath_member_value!($field, $wire, $presence) ),+
                })
            }
        }
    };
}
