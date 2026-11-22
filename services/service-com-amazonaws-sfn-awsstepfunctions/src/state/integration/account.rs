// SPDX-License-Identifier: CC0-1.0

//! AWS Account Management's own wire facts, reached for the reason Amazon S3's and Amazon
//! CloudFront's are: a caller names an operation of it through the AWS SDK integration
//! FAMILY, and nothing else.
//!
//! AWS publishes no OPTIMIZED integration for this service, so
//! `arn:aws:states:::account:<action>` names no `Task` state's work — only the family's
//! own `aws-sdk:account:<operation>` does, which is also how Amazon EventBridge
//! Scheduler's UNIVERSAL target names a call ([`super::aws_sdk_call`]). Its entry is
//! therefore carried in [`super::AWS_SDK_APIS`] rather than in [`super::APIS`], the table
//! the optimized family is looked up in, and lives here with the REST bindings of the
//! operations those callers make, so the one concern sits in one file.
//!
//! Its protocol is `restJson1`, and none of the operations bound below binds a member as
//! its `httpPayload` or to the path, the query or a header: each request carries the
//! operation's own input as the document body, and each answer's body IS that operation's
//! output document ([`super::Body::Document`]).

use super::*;

/// AWS Account Management, for the calls this crate makes to it: a `Task` state whose
/// `Resource` names one of its operations through the AWS SDK integration family, and an
/// EventBridge Scheduler universal target that names one the same way.
///
/// `services/service-com-amazonaws-account-account/smithy-model/
/// com-amazonaws-account-account.json` (`smithy_query(query="service_info")`): restJson1,
/// sigv4Name `account`, arnNamespace `account`, endpoint host
/// `account.<region>.<dnsSuffix>`, sdkId `Account`.
pub(super) static ACCOUNT_API: Api = Api {
    namespace: ACCOUNT_NAMESPACE,
    action_prefix: None,
    signing_name: ACCOUNT_NAMESPACE,
    endpoint_prefix: ACCOUNT_NAMESPACE,
    wire: Wire::Rest(ACCOUNT_REST),
    sdk_id: "Account",
    error_prefix: "Account",
    // The AWS SDK names the service `Account`, which its own `Account` client carries.
    sdk_error_prefix: "Account",
    // The client class `AWSAccountClient`, whose base exception is `AWSAccountException`.
    // Only the OPTIMIZED family's rendering reads these two ([`sdk_exception_message`]),
    // and no optimized `Resource` names this service — they are stated because an entry
    // states the service's own names rather than only the ones one caller reaches.
    sdk_client: "AWSAccount",
    sdk_exception: "AWSAccountException",
    // Every operation of this service that models a refusal of an unauthorized call
    // models `AccessDeniedException` with HTTP 403, which is also how its front door
    // answers one ([`ACCOUNT_ACCESS_DENIED`]).
    access_denied_status: TARGET_FORBIDDEN_STATUS,
    access_denied_code: COMMON_ACCESS_DENIED_CODE,
    lowercases_refused_action: false,
    // `ListRegions`' input: `AccountId`, `MaxResults`, `RegionOptStatusContains`; its
    // output: `Regions`, `NextToken`. No member of this model carries a `jsonName`, so
    // the wire spelling of each is the model's own.
    member_case: MemberCase::Pascal,
    query_compatible: false,
    models_errors: true,
};

/// This service's ARN namespace, its signing name and its endpoint prefix, which are the
/// same label.
pub(super) const ACCOUNT_NAMESPACE: &str = "account";

/// The refusal the reads of this service model for a call its caller is not authorized to
/// make: the `AccessDeniedException` shape, answered with HTTP 403
/// (`smithy_query(query="operation",operation="ListRegions,GetRegionOptStatus,
/// GetAccountInformation,GetContactInformation",model_path="services/
/// service-com-amazonaws-account-account/smithy-model/com-amazonaws-account-account.json")`:
/// `AccessDeniedException`, httpError 403). The shape is already spelled with the
/// `Exception` suffix a class name ends in, so the class its SDK resolves the answer to is
/// that shape's own name.
pub(super) const ACCOUNT_ACCESS_DENIED: Denied = Denied {
    exception: COMMON_ACCESS_DENIED_CODE,
    code: COMMON_ACCESS_DENIED_CODE,
    status: TARGET_FORBIDDEN_STATUS,
};

/// The operation that lists an account's Regions, as that model spells it.
pub(super) const LIST_REGIONS_OPERATION: &str = "ListRegions";
/// See [`LIST_REGIONS_OPERATION`].
const GET_REGION_OPT_STATUS_OPERATION: &str = "GetRegionOptStatus";
/// See [`LIST_REGIONS_OPERATION`].
const GET_ACCOUNT_INFORMATION_OPERATION: &str = "GetAccountInformation";
/// See [`LIST_REGIONS_OPERATION`].
const GET_CONTACT_INFORMATION_OPERATION: &str = "GetContactInformation";
/// See [`LIST_REGIONS_OPERATION`].
const ENABLE_REGION_OPERATION: &str = "EnableRegion";
/// See [`LIST_REGIONS_OPERATION`].
const DISABLE_REGION_OPERATION: &str = "DisableRegion";

/// The REST bindings of the AWS Account Management operations a `Task` state's integration
/// call is made for.
///
/// Read from that service's own model with
/// `smithy_query(query="operation",operation="ListRegions,GetRegionOptStatus,
/// GetAccountInformation,GetContactInformation,EnableRegion,DisableRegion",
/// model_path="services/service-com-amazonaws-account-account/smithy-model/
/// com-amazonaws-account-account.json")`: every one of them is `http` POST on a path of
/// its own naming the action (`/listRegions`, `/getRegionOptStatus`, …) with NO
/// `httpLabel`, `httpQuery`, `httpHeader` or `httpPayload` member and no output member
/// bound to the status or to a header — so each request carries the operation's input as
/// the document body and each answer's body IS its output document. An operation of this
/// service with no entry is one no call is made for ([`super::read`]).
pub(super) const ACCOUNT_REST: &[Rest] = &[
    Rest {
        operation: LIST_REGIONS_OPERATION,
        method: "POST",
        uri: "/listRegions",
        request_body: Body::Document,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: GET_REGION_OPT_STATUS_OPERATION,
        method: "POST",
        uri: "/getRegionOptStatus",
        request_body: Body::Document,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: GET_ACCOUNT_INFORMATION_OPERATION,
        method: "POST",
        uri: "/getAccountInformation",
        request_body: Body::Document,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: GET_CONTACT_INFORMATION_OPERATION,
        method: "POST",
        uri: "/getContactInformation",
        request_body: Body::Document,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    // The two writes of a Region's opt-in status, whose output is `Unit`: the answer
    // carries no member at all, which is the empty result a task reports for them.
    Rest {
        operation: ENABLE_REGION_OPERATION,
        method: "POST",
        uri: "/enableRegion",
        request_body: Body::Document,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: DISABLE_REGION_OPERATION,
        method: "POST",
        uri: "/disableRegion",
        request_body: Body::Document,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The AWS SDK family's `Resource` names this service by its SDK id lower-cased, and
    /// an operation whose bindings are stated is one a call is made for.
    #[test]
    fn the_family_names_this_service_by_its_sdk_id() {
        assert_eq!(
            ACCOUNT_API.sdk_token(),
            ACCOUNT_NAMESPACE,
            "the family's token for this service is its SDK id lower-cased, got {:?}",
            ACCOUNT_API.sdk_token()
        );
        // (case, the operation, whether a request to it is bound)
        let rows: [(&str, &str, bool); 4] = [
            ("the listing the recorded workflow calls", LIST_REGIONS_OPERATION, true),
            ("another read of the same service", GET_ACCOUNT_INFORMATION_OPERATION, true),
            ("a write of a Region's opt-in status", ENABLE_REGION_OPERATION, true),
            ("an operation of the service with no binding", "PutAccountName", false),
        ];
        for (case, operation, expected) in rows {
            let bound = ACCOUNT_REST.iter().any(|rest| rest.operation == operation);
            assert_eq!(
                bound, expected,
                "case {case:?}: expected bound={expected}, got {bound}"
            );
        }
    }
}
