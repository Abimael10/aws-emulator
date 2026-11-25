// SPDX-License-Identifier: CC0-1.0

//! The per-id failure entry a batch campaign-state read reports, and the one member of it
//! the on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error,
//! so a member the service really puts on the wire but the model omits cannot be declared
//! through the derive at all. A batch state read reports each id it holds no campaign under
//! with the failure CODE the model declares and, beside it, the service's own explanation of
//! that failure naming the id — and the on-disk `FailedCampaignStateResponse`
//! (`smithy_query(query="shape", shape="FailedCampaignStateResponse")`) declares only
//! `campaignId` and `failureCode`, which is why this entry's wire form is written out here.
//!
//! The model's own members keep the order and the indices the model gives them, and the
//! member it does not declare is appended LAST with its shape id in a namespace of its own
//! ([`RECORDED_NAMESPACE`]), so it can neither claim to be modelled nor collide with a
//! member the model may declare later — a model refresh only has to move it into place and
//! put the derive back. The ids never leave the process: the codec writes a member's NAME as
//! the key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and this
//! entry is output-only), the `@http` chain (baked on only for an operation's input) and
//! `@jsonName` (each member's wire name IS its member name).
//!
//! Nothing here decides a VALUE: what each member carries is the handler's
//! ([`crate::ops::get_campaign_state_batch`]), which fills the explanation from the id the
//! request asked about through [`crate::state::campaign_absent_message`].

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_schema_server::DeserializableStruct;

/// The namespace the member the on-disk model does not declare sits in: NOT this service's
/// own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.connectcampaignsv2.recorded";

/// This service's own namespace, which the two members the model declares sit in.
const MODELLED_NAMESPACE: &str = "com.amazonaws.connectcampaignsv2";

/// One id of a batch campaign-state read the service could not report a state for: the id
/// it was asked about, the modelled code for why, and the explanation of that failure in
/// words.
#[derive(Debug, Clone, Default)]
pub struct FailedCampaignStateResponse {
    /// `campaignId` — the id of the request's `campaignIds` list this entry answers for.
    pub campaign_id: Option<String>,
    /// `failureCode` — a value of the model's `GetCampaignStateBatchFailureCode` catalog.
    pub failure_code: Option<String>,
    /// `failureReason` — the explanation of this failure in words, naming the id it is
    /// about. The member the on-disk model does not declare.
    pub failure_reason: Option<String>,
}

#[allow(non_upper_case_globals)]
impl FailedCampaignStateResponse {
    /// `campaignId`, the model's first member.
    pub const M_CAMPAIGN_ID: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.connectcampaignsv2#FailedCampaignStateResponse$campaignId",
            MODELLED_NAMESPACE,
            "campaignId",
        ),
        ShapeType::String,
        "campaignId",
        0,
    );

    /// `failureCode`, the model's second member.
    pub const M_FAILURE_CODE: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.connectcampaignsv2#FailedCampaignStateResponse$failureCode",
            MODELLED_NAMESPACE,
            "failureCode",
        ),
        ShapeType::String,
        "failureCode",
        1,
    );

    /// `failureReason`, the one member the model does not declare, appended after the
    /// model's own two (the index is its position in the member list the wire carries) and
    /// in [`RECORDED_NAMESPACE`].
    pub const M_FAILURE_REASON: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.connectcampaignsv2.recorded#FailedCampaignStateResponse$failureReason",
            RECORDED_NAMESPACE,
            "failureReason",
        ),
        ShapeType::String,
        "failureReason",
        2,
    );

    /// The entry's shape, naming its members in the order the wire carries them.
    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.connectcampaignsv2#FailedCampaignStateResponse",
            MODELLED_NAMESPACE,
            "FailedCampaignStateResponse",
        ),
        ShapeType::Structure,
        &[
            &Self::M_CAMPAIGN_ID,
            &Self::M_FAILURE_CODE,
            &Self::M_FAILURE_REASON,
        ],
    );
}

/// The entry's shape as a static, as the derive emits one for every structure it generates,
/// so a caller that needs the shape by reference reads the same one the codec writes
/// against.
pub static FAILED_CAMPAIGN_STATE_RESPONSE_SCHEMA: &Schema = &FailedCampaignStateResponse::SCHEMA;

impl SerializableStruct for FailedCampaignStateResponse {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        // Each member is written only when it is there, exactly as the derive writes an
        // optional one.
        if let Some(campaign_id) = self.campaign_id.as_ref() {
            s.write_string(&Self::M_CAMPAIGN_ID, campaign_id.as_str())?;
        }
        if let Some(failure_code) = self.failure_code.as_ref() {
            s.write_string(&Self::M_FAILURE_CODE, failure_code.as_str())?;
        }
        if let Some(failure_reason) = self.failure_reason.as_ref() {
            s.write_string(&Self::M_FAILURE_REASON, failure_reason.as_str())?;
        }
        Ok(())
    }
}

impl DeserializableStruct for FailedCampaignStateResponse {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut campaign_id = None;
        let mut failure_code = None;
        let mut failure_reason = None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => campaign_id = Some(d.read_string(&Self::M_CAMPAIGN_ID)?),
                Some(1) => failure_code = Some(d.read_string(&Self::M_FAILURE_CODE)?),
                Some(2) => failure_reason = Some(d.read_string(&Self::M_FAILURE_REASON)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            campaign_id,
            failure_code,
            failure_reason,
        })
    }
}
