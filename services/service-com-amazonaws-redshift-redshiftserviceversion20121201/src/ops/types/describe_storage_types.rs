// SPDX-License-Identifier: CC0-1.0

//! Type definitions reached only by the `describe_storage` operation.
//!
//! HAND-AUTHORED, not scaffolded: `DescribeStorage` binds Smithy's `Unit` input,
//! which the op-stub generator declines to type (see `src/ops/SKIPPED_STUBS.md`),
//! so its shapes are declared here beside the generated ones. Each
//! structure/union is declared exactly once across this service's `types/`
//! modules, and `types/mod.rs` re-exports them all.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-redshift-redshiftserviceversion20121201.json");

/// `DescribeStorage` has no input members: the model binds it to
/// `smithy.api#Unit`, a PRELUDE shape that is not in this service's model file.
/// The legacy `structure = "..."` key declares the shape id without resolving it
/// against the model, and the macro's empty-struct path emits the schema, the
/// `DESCRIBE_STORAGE_INPUT_SCHEMA` static and the serialize/deserialize impls
/// `Operation::Input` requires from the id alone.
#[smithy_macros::smithy(structure = "smithy.api#Unit")]
#[derive(Debug, Clone, Default)]
pub struct DescribeStorageInput {}

/// `DescribeStorage`'s output: the `CustomerStorageMessage` shape.
///
/// Hand-authored like the input, because the generator emitted neither: the
/// operation's `Unit` input is what the generator declined, and an output shape
/// needs its schema and serializer beside it.
///
/// Both members are Smithy `double`s, so they reach the wire in the decimal form
/// AWS renders one in (`0.0`, never `0`) — which the fleet's one renderer for
/// that rule applies to every `double` of every shape this service serves
/// through it, not to these two (`crate::service::decimal_floats`).
#[derive(Debug, Clone, Default)]
pub struct DescribeStorageOutput {
    pub total_backup_size_in_mega_bytes: Option<f64>,
    pub total_provisioned_storage_in_mega_bytes: Option<f64>,
}

#[allow(non_upper_case_globals)]
impl DescribeStorageOutput {
    const NAMESPACE: &'static str = "com.amazonaws.redshift";

    pub const M_TOTAL_BACKUP_SIZE_IN_MEGA_BYTES: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.redshift#CustomerStorageMessage$TotalBackupSizeInMegaBytes",
                Self::NAMESPACE,
                "TotalBackupSizeInMegaBytes",
            ),
            ::aws_smithy_schema::ShapeType::Double,
            "TotalBackupSizeInMegaBytes",
            0,
        );

    pub const M_TOTAL_PROVISIONED_STORAGE_IN_MEGA_BYTES: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.redshift#CustomerStorageMessage$TotalProvisionedStorageInMegaBytes",
                Self::NAMESPACE,
                "TotalProvisionedStorageInMegaBytes",
            ),
            ::aws_smithy_schema::ShapeType::Double,
            "TotalProvisionedStorageInMegaBytes",
            1,
        );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.redshift#CustomerStorageMessage",
            Self::NAMESPACE,
            "CustomerStorageMessage",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_TOTAL_BACKUP_SIZE_IN_MEGA_BYTES,
            &Self::M_TOTAL_PROVISIONED_STORAGE_IN_MEGA_BYTES,
        ],
    );
}

pub static DESCRIBE_STORAGE_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema =
    &DescribeStorageOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DescribeStorageOutput {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        // Each member is written as the double the model declares it, exactly as
        // the generated serializer of every other shape writes one: the
        // element's name, order, schema and type are all the model's.
        if let Some(value) = self.total_backup_size_in_mega_bytes {
            s.write_double(&Self::M_TOTAL_BACKUP_SIZE_IN_MEGA_BYTES, value)?;
        }
        if let Some(value) = self.total_provisioned_storage_in_mega_bytes {
            s.write_double(&Self::M_TOTAL_PROVISIONED_STORAGE_IN_MEGA_BYTES, value)?;
        }
        Ok(())
    }
}
