// SPDX-License-Identifier: CC0-1.0

//! The schema and the codec for [`Instance`], the one structure of this service whose WIRE
//! form carries members the on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the model's member list, and it admits
//! EXACTLY that list: a Rust field with no member behind it is a compile error, so a member
//! the service really reports but the on-disk model omits cannot be declared through the
//! derive at all. `Instance` has two such members — `primaryDomain` and `idcPortalDomain`,
//! the instance's WebApp hostname and its IAM Identity Center portal hostname, which every
//! recorded response of every operation answering this shape carries and the model declares
//! nowhere. So the items below are exactly what the derive emits for this shape — one
//! `M_<MEMBER>` member schema per member (index = that member's position in the member
//! list), the `SCHEMA` naming them in that order, the `INSTANCE_SCHEMA` static and the
//! `SerializableStruct`/`DeserializableStruct` pair — written out so the member list can be
//! two entries longer than the model's.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="com.amazonaws.supplychain#Instance")` lists them in),
//! and the two it does not declare are appended LAST with their shape ids in a namespace of
//! their own, so neither can claim to be modelled nor collide with a member the model may
//! declare later — and a later model refresh only has to move them into place and put the
//! derive back on the struct.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here:
//!
//! * Constraint traits (`@length`, `@pattern`, …). The validating walk that enforces them
//!   runs on operation INPUTS only, and `Instance` is output-only: no operation of this
//!   service takes it as an input, so the derive's captured constraints are inert on it.
//! * The `@http` chain, which is baked onto a shape only when it is an operation's INPUT.
//! * `@xmlName`/`@jsonName`. No member below declares either, and this service's sole
//!   protocol is restJson1, so each member's wire name IS its Smithy member name.
//! * `@timestampFormat`. `createdTime` and `lastModifiedTime` target the prelude
//!   `smithy.api#Timestamp` with no format trait, so both render in the protocol's default
//!   form — the same form the derive produced for them.

use super::shared_types::Instance;

/// The namespace the two members the model does not declare sit in, so neither can be
/// mistaken for a member of `com.amazonaws.supplychain#Instance` as the model has it.
const RECORDED_NAMESPACE: &str = "com.amazonaws.supplychain.recorded";

/// The service's own namespace: where the eleven members the model DOES declare sit.
const NAMESPACE: &str = "com.amazonaws.supplychain";

#[allow(non_upper_case_globals)]
impl Instance {
    pub const M_INSTANCE_ID: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.supplychain#Instance$instanceId",
            NAMESPACE,
            "instanceId",
        ),
        ::aws_smithy_schema::ShapeType::String,
        "instanceId",
        0,
    );

    pub const M_AWS_ACCOUNT_ID: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#Instance$awsAccountId",
                NAMESPACE,
                "awsAccountId",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "awsAccountId",
            1,
        );

    pub const M_STATE: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.supplychain#Instance$state",
            NAMESPACE,
            "state",
        ),
        // `InstanceState` is an `enum` shape, which is string-encoded on the wire.
        ::aws_smithy_schema::ShapeType::String,
        "state",
        2,
    );

    pub const M_ERROR_MESSAGE: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#Instance$errorMessage",
                NAMESPACE,
                "errorMessage",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "errorMessage",
            3,
        );

    pub const M_WEB_APP_DNS_DOMAIN: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#Instance$webAppDnsDomain",
                NAMESPACE,
                "webAppDnsDomain",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "webAppDnsDomain",
            4,
        );

    pub const M_CREATED_TIME: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.supplychain#Instance$createdTime",
            NAMESPACE,
            "createdTime",
        ),
        ::aws_smithy_schema::ShapeType::Timestamp,
        "createdTime",
        5,
    );

    pub const M_LAST_MODIFIED_TIME: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#Instance$lastModifiedTime",
                NAMESPACE,
                "lastModifiedTime",
            ),
            ::aws_smithy_schema::ShapeType::Timestamp,
            "lastModifiedTime",
            6,
        );

    pub const M_INSTANCE_NAME: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#Instance$instanceName",
                NAMESPACE,
                "instanceName",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "instanceName",
            7,
        );

    pub const M_INSTANCE_DESCRIPTION: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#Instance$instanceDescription",
                NAMESPACE,
                "instanceDescription",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "instanceDescription",
            8,
        );

    pub const M_KMS_KEY_ARN: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.supplychain#Instance$kmsKeyArn",
            NAMESPACE,
            "kmsKeyArn",
        ),
        ::aws_smithy_schema::ShapeType::String,
        "kmsKeyArn",
        9,
    );

    pub const M_VERSION_NUMBER: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#Instance$versionNumber",
                NAMESPACE,
                "versionNumber",
            ),
            ::aws_smithy_schema::ShapeType::Double,
            "versionNumber",
            10,
        );

    /// The instance's WebApp hostname. RECORDED, not modelled — hence the namespace; see
    /// the module note.
    pub const M_PRIMARY_DOMAIN: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain.recorded#Instance$primaryDomain",
                RECORDED_NAMESPACE,
                "primaryDomain",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "primaryDomain",
            11,
        );

    /// The instance's IAM Identity Center portal hostname. RECORDED, not modelled — hence
    /// the namespace; see the module note.
    pub const M_IDC_PORTAL_DOMAIN: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain.recorded#Instance$idcPortalDomain",
                RECORDED_NAMESPACE,
                "idcPortalDomain",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "idcPortalDomain",
            12,
        );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.supplychain#Instance",
            NAMESPACE,
            "Instance",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_INSTANCE_ID,
            &Self::M_AWS_ACCOUNT_ID,
            &Self::M_STATE,
            &Self::M_ERROR_MESSAGE,
            &Self::M_WEB_APP_DNS_DOMAIN,
            &Self::M_CREATED_TIME,
            &Self::M_LAST_MODIFIED_TIME,
            &Self::M_INSTANCE_NAME,
            &Self::M_INSTANCE_DESCRIPTION,
            &Self::M_KMS_KEY_ARN,
            &Self::M_VERSION_NUMBER,
            &Self::M_PRIMARY_DOMAIN,
            &Self::M_IDC_PORTAL_DOMAIN,
        ],
    );
}

/// `Instance`'s schema, under the name the derive would have given it, so a caller that
/// names the shape's schema reaches the same one the codec uses.
pub static INSTANCE_SCHEMA: &::aws_smithy_schema::Schema = &Instance::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for Instance {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        // A `@required` member is written always, an optional one only when it is there:
        // the derive's own split, which is what keeps an absent member off the wire
        // instead of putting an empty value on it.
        s.write_string(&Self::M_INSTANCE_ID, self.instance_id.as_str())?;
        s.write_string(&Self::M_AWS_ACCOUNT_ID, self.aws_account_id.as_str())?;
        s.write_string(&Self::M_STATE, self.state.as_str())?;
        if let Some(value) = self.error_message.as_ref() {
            s.write_string(&Self::M_ERROR_MESSAGE, value.as_str())?;
        }
        if let Some(value) = self.web_app_dns_domain.as_ref() {
            s.write_string(&Self::M_WEB_APP_DNS_DOMAIN, value.as_str())?;
        }
        if let Some(value) = self.created_time.as_ref() {
            s.write_timestamp(&Self::M_CREATED_TIME, value)?;
        }
        if let Some(value) = self.last_modified_time.as_ref() {
            s.write_timestamp(&Self::M_LAST_MODIFIED_TIME, value)?;
        }
        if let Some(value) = self.instance_name.as_ref() {
            s.write_string(&Self::M_INSTANCE_NAME, value.as_str())?;
        }
        if let Some(value) = self.instance_description.as_ref() {
            s.write_string(&Self::M_INSTANCE_DESCRIPTION, value.as_str())?;
        }
        if let Some(value) = self.kms_key_arn.as_ref() {
            s.write_string(&Self::M_KMS_KEY_ARN, value.as_str())?;
        }
        if let Some(value) = self.version_number.as_ref() {
            s.write_double(&Self::M_VERSION_NUMBER, *value)?;
        }
        if let Some(value) = self.primary_domain.as_ref() {
            s.write_string(&Self::M_PRIMARY_DOMAIN, value.as_str())?;
        }
        if let Some(value) = self.idc_portal_domain.as_ref() {
            s.write_string(&Self::M_IDC_PORTAL_DOMAIN, value.as_str())?;
        }
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for Instance {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut instance_id = ::std::option::Option::None;
        let mut aws_account_id = ::std::option::Option::None;
        let mut state = ::std::option::Option::None;
        let mut error_message = ::std::option::Option::None;
        let mut web_app_dns_domain = ::std::option::Option::None;
        let mut created_time = ::std::option::Option::None;
        let mut last_modified_time = ::std::option::Option::None;
        let mut instance_name = ::std::option::Option::None;
        let mut instance_description = ::std::option::Option::None;
        let mut kms_key_arn = ::std::option::Option::None;
        let mut version_number = ::std::option::Option::None;
        let mut primary_domain = ::std::option::Option::None;
        let mut idc_portal_domain = ::std::option::Option::None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    instance_id = ::std::option::Option::Some(d.read_string(&Self::M_INSTANCE_ID)?);
                }
                ::std::option::Option::Some(1) => {
                    aws_account_id =
                        ::std::option::Option::Some(d.read_string(&Self::M_AWS_ACCOUNT_ID)?);
                }
                ::std::option::Option::Some(2) => {
                    state = ::std::option::Option::Some(d.read_string(&Self::M_STATE)?);
                }
                ::std::option::Option::Some(3) => {
                    error_message =
                        ::std::option::Option::Some(d.read_string(&Self::M_ERROR_MESSAGE)?);
                }
                ::std::option::Option::Some(4) => {
                    web_app_dns_domain =
                        ::std::option::Option::Some(d.read_string(&Self::M_WEB_APP_DNS_DOMAIN)?);
                }
                ::std::option::Option::Some(5) => {
                    created_time =
                        ::std::option::Option::Some(d.read_timestamp(&Self::M_CREATED_TIME)?);
                }
                ::std::option::Option::Some(6) => {
                    last_modified_time =
                        ::std::option::Option::Some(d.read_timestamp(&Self::M_LAST_MODIFIED_TIME)?);
                }
                ::std::option::Option::Some(7) => {
                    instance_name =
                        ::std::option::Option::Some(d.read_string(&Self::M_INSTANCE_NAME)?);
                }
                ::std::option::Option::Some(8) => {
                    instance_description =
                        ::std::option::Option::Some(d.read_string(&Self::M_INSTANCE_DESCRIPTION)?);
                }
                ::std::option::Option::Some(9) => {
                    kms_key_arn = ::std::option::Option::Some(d.read_string(&Self::M_KMS_KEY_ARN)?);
                }
                ::std::option::Option::Some(10) => {
                    version_number =
                        ::std::option::Option::Some(d.read_double(&Self::M_VERSION_NUMBER)?);
                }
                ::std::option::Option::Some(11) => {
                    primary_domain =
                        ::std::option::Option::Some(d.read_string(&Self::M_PRIMARY_DOMAIN)?);
                }
                ::std::option::Option::Some(12) => {
                    idc_portal_domain =
                        ::std::option::Option::Some(d.read_string(&Self::M_IDC_PORTAL_DOMAIN)?);
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            // The three `@required` members are refused by name when the wire carries
            // none, as the derive refuses them; every other member stays absent.
            instance_id: instance_id
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("instanceId"))?,
            aws_account_id: aws_account_id
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("awsAccountId"))?,
            state: state.ok_or_else(|| ::aws_smithy_schema_server::missing_member("state"))?,
            error_message,
            web_app_dns_domain,
            created_time,
            last_modified_time,
            instance_name,
            instance_description,
            kms_key_arn,
            version_number,
            primary_domain,
            idc_portal_domain,
        })
    }
}
