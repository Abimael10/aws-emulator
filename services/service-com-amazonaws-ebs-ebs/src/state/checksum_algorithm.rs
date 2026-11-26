// SPDX-License-Identifier: CC0-1.0

//! The modelled FORM of the `ChecksumAlgorithm` header member, stated once for every
//! operation that carries it.
//!
//! `PutSnapshotBlock` and `CompleteSnapshot` both bind this member to
//! `x-amz-Checksum-Algorithm` and both take it off the request boundary
//! (`skip_validation`), because an algorithm this service does not support is answered with
//! the service's OWN modelled `ValidationException` — the shape carrying the `Message` and
//! the `Reason` a client reads, which the boundary's synthetic envelope carries neither of.
//!
//! Taking the member off the boundary takes its modelled constraint traits off the
//! generated member schema with it, so there is none left at the boundary to read them
//! from: a value outside the member's declared FORM would otherwise reach the service's
//! value judgement, which is about which algorithms it supports and not about what a
//! `ChecksumAlgorithm` may look like. So the traits are restated here, once, and both
//! operations write this member schema into their own constraint walk — the same walk, the
//! same clause and the same ceiling on how much of a rejected value is echoed that every
//! other member they judge gets.
//!
//! One module for the member rather than one per operation: both bind the SAME target
//! shape, so the constraints and the clause they produce are the same, and a second copy
//! could only drift from the first.

use std::sync::LazyLock;

use ::aws_smithy_schema::{Schema, ShapeId, ShapeType, TraitMap};
use ::aws_smithy_schema_server::constraints::{LengthTrait, PatternTrait};

/// The modelled `@length` of `com.amazonaws.ebs#ChecksumAlgorithm`
/// (`smithy_query(query="shape",shape="com.amazonaws.ebs#ChecksumAlgorithm")` reports
/// `@length` 0..32), which every member targeting that shape carries.
pub const CHECKSUM_ALGORITHM_LENGTH: (i64, i64) = (0, 32);

/// The modelled `@pattern` of `com.amazonaws.ebs#ChecksumAlgorithm`
/// (`^[A-Za-z0-9]+$`, from the same shape).
pub const CHECKSUM_ALGORITHM_PATTERN: &str = "^[A-Za-z0-9]+$";

/// The traits the model declares on `ChecksumAlgorithm`, restated because
/// `skip_validation` takes them off the generated member schemas of both operations that
/// carry the member.
static CHECKSUM_ALGORITHM_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
    let mut traits = TraitMap::new();
    traits.insert(Box::new(PatternTrait::new(CHECKSUM_ALGORITHM_PATTERN)));
    traits.insert(Box::new(LengthTrait::new(
        Some(CHECKSUM_ALGORITHM_LENGTH.0),
        Some(CHECKSUM_ALGORITHM_LENGTH.1),
    )));
    traits
});

/// `ChecksumAlgorithm` as a MEMBER schema carrying [`CHECKSUM_ALGORITHM_TRAITS`], so the
/// check is the framework's walk rather than a handler's own: the violation it reports —
/// its kind, its lower-camelled path (`checksumAlgorithm`) and its ceiling on how much of
/// the rejected value is echoed — is the framework's too, exactly as for the sibling
/// members each operation judges beside this one.
///
/// Written into the operation's own walk rather than validated here on its own, so a
/// request breaking several of an operation's constraints at once is reported in ONE
/// envelope, as the boundary would have reported it.
///
/// The shape id is the TARGET shape's rather than one input shape's member id (the sibling
/// schemas, each used by a single operation, name theirs): two input shapes bind this
/// member, so neither member id would be true of both. Nothing reads it — the path a clause
/// states comes from the member name below — so what it names is the shape whose
/// constraints these are.
pub static CHECKSUM_ALGORITHM_SCHEMA: LazyLock<Schema> = LazyLock::new(|| {
    Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.ebs#ChecksumAlgorithm",
            "com.amazonaws.ebs",
            "ChecksumAlgorithm",
        ),
        ShapeType::String,
        "ChecksumAlgorithm",
        0,
    )
    .with_traits(&CHECKSUM_ALGORITHM_TRAITS)
});
