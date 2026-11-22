// SPDX-License-Identifier: CC0-1.0

//! Amazon API Gateway V2's own wire facts, reached for the reason AWS Account
//! Management's are: a caller names an operation of it through the AWS SDK integration
//! FAMILY, and nothing else.
//!
//! AWS publishes no OPTIMIZED integration for this control plane, so
//! `arn:aws:states:::apigatewayv2:<action>` names no `Task` state's work — only the
//! family's own `aws-sdk:apigatewayv2:<operation>` does, which is also how Amazon
//! EventBridge Scheduler's UNIVERSAL target names a call ([`super::aws_sdk_call`]). Its
//! entry is therefore carried in [`super::AWS_SDK_APIS`] rather than in [`super::APIS`],
//! the table the optimized family is looked up in, and lives here with the REST bindings
//! of the operations those callers make.
//!
//! Its protocol is `restJson1`, and its model is the one here whose members are SPELLED
//! one way and carried on the wire another: every member of it declares a `jsonName` that
//! is the member's own name lower-camelled (`MaxResults` -> `maxResults`, `Items` ->
//! `items`), so the wire spelling this entry states is lowerCamel
//! ([`super::MemberCase`]) — and the path labels and query parameters bound below name
//! each member as the WIRE spells it, because that is the document the request is built
//! from ([`super::Rest::wired`]).

use super::*;

/// Amazon API Gateway V2's control plane, for the calls this crate makes to it: a `Task`
/// state whose `Resource` names one of its operations through the AWS SDK integration
/// family, and an EventBridge Scheduler universal target that names one the same way.
///
/// `services/service-com-amazonaws-apigatewayv2-apigatewayv2/smithy-model/
/// com-amazonaws-apigatewayv2-apigatewayv2.json`
/// (`smithy_query(query="service_info")`): restJson1, sigv4Name `apigateway`,
/// endpointPrefix `apigateway`, arnNamespace `apigateway` ([`APIGATEWAY_NAMESPACE`]), sdkId
/// `ApiGatewayV2` — the one entry whose signing name and endpoint are NOT its SDK id, the
/// two API Gateway control planes answering on one endpoint and being told apart by their
/// paths (this one's are bound under `/v2/…`).
pub(super) static APIGATEWAYV2_API: Api = Api {
    namespace: APIGATEWAY_NAMESPACE,
    action_prefix: None,
    signing_name: APIGATEWAY_NAMESPACE,
    endpoint_prefix: APIGATEWAY_NAMESPACE,
    wire: Wire::Rest(APIGATEWAYV2_REST),
    sdk_id: "ApiGatewayV2",
    error_prefix: "ApiGatewayV2",
    // The AWS SDK names the service `ApiGatewayV2`, which its own `ApiGatewayV2` client
    // carries.
    sdk_error_prefix: "ApiGatewayV2",
    // The client class `AmazonApiGatewayV2Client`, whose base exception is
    // `AmazonApiGatewayV2Exception`. Only the OPTIMIZED family's rendering reads these
    // two ([`sdk_exception_message`]), and no optimized `Resource` names this service.
    sdk_client: "AmazonApiGatewayV2",
    sdk_exception: "AmazonApiGatewayV2Exception",
    // No operation of this model declares an access error of its own, so an unauthorized
    // call is answered by the service's front door — API Gateway answers one as a
    // Forbidden under the AWS-common code.
    access_denied_status: TARGET_FORBIDDEN_STATUS,
    access_denied_code: COMMON_ACCESS_DENIED_CODE,
    lowercases_refused_action: false,
    // `GetApis`' input members `MaxResults` and `NextToken` carry the `jsonName`s
    // `maxResults` and `nextToken`, and its output's `Items` carries `items`: the wire
    // spelling of every member of this model is its name lower-camelled.
    member_case: MemberCase::LowerCamel,
    query_compatible: false,
    models_errors: true,
};

/// The operation that lists an account's apis, as that model spells it.
pub(super) const GET_APIS_OPERATION: &str = "GetApis";
/// See [`GET_APIS_OPERATION`].
pub(super) const GET_API_OPERATION: &str = "GetApi";
/// See [`GET_APIS_OPERATION`].
pub(super) const CREATE_API_OPERATION: &str = "CreateApi";
/// See [`GET_APIS_OPERATION`].
const DELETE_API_OPERATION: &str = "DeleteApi";
/// See [`GET_APIS_OPERATION`].
const GET_ROUTES_OPERATION: &str = "GetRoutes";
/// See [`GET_APIS_OPERATION`].
const GET_STAGES_OPERATION: &str = "GetStages";

/// The REST bindings of the Amazon API Gateway V2 operations a `Task` state's integration
/// call is made for.
///
/// Read from that service's own model with
/// `smithy_query(query="operation",operation="GetApis,GetApi,CreateApi,DeleteApi,
/// GetRoutes,GetStages",model_path="services/
/// service-com-amazonaws-apigatewayv2-apigatewayv2/smithy-model/
/// com-amazonaws-apigatewayv2-apigatewayv2.json")`: `GetApis` is `http` GET `/v2/apis`
/// with `MaxResults` and `NextToken` `httpQuery` members, `GetApi` and `DeleteApi` are
/// GET and DELETE `/v2/apis/{ApiId}` over that one `httpLabel`, `CreateApi` is POST
/// `/v2/apis` (code 201) with every member in the body, and `GetRoutes` and `GetStages`
/// are GET under the api's own path with the same two query members beside the label. None
/// of them binds a member as its `httpPayload`, and none binds an output member to the
/// status or to a header, so each answer's body IS the operation's output document. An
/// operation of this service with no entry is one no call is made for ([`super::read`]).
///
/// Each label and each parameter names its member as the WIRE spells it, which for this
/// model is the member's name lower-camelled ([`APIGATEWAYV2_API`]).
pub(super) const APIGATEWAYV2_REST: &[Rest] = &[
    Rest {
        operation: GET_APIS_OPERATION,
        method: "GET",
        uri: "/v2/apis",
        request_body: Body::Empty,
        headers: &[],
        query: PAGING_QUERY,
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: GET_API_OPERATION,
        method: "GET",
        uri: "/v2/apis/{apiId}",
        request_body: Body::Empty,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: CREATE_API_OPERATION,
        method: "POST",
        uri: "/v2/apis",
        request_body: Body::Document,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    // The delete's output is `Unit`: the answer carries no member at all, which is the
    // empty result a task reports for it.
    Rest {
        operation: DELETE_API_OPERATION,
        method: "DELETE",
        uri: "/v2/apis/{apiId}",
        request_body: Body::Empty,
        headers: &[],
        query: &[],
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: GET_ROUTES_OPERATION,
        method: "GET",
        uri: "/v2/apis/{apiId}/routes",
        request_body: Body::Empty,
        headers: &[],
        query: PAGING_QUERY,
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
    Rest {
        operation: GET_STAGES_OPERATION,
        method: "GET",
        uri: "/v2/apis/{apiId}/stages",
        request_body: Body::Empty,
        headers: &[],
        query: PAGING_QUERY,
        response_body: Body::Document,
        response_status: "",
        response_headers: &[],
    },
];

/// The two `httpQuery` members every listing of this service pages with, each under the
/// parameter name its own binding states (`maxResults`, `nextToken`) — which for this
/// model is also the wire spelling of the member.
const PAGING_QUERY: &[(&str, &str)] = &[("maxResults", "maxResults"), ("nextToken", "nextToken")];

#[cfg(test)]
mod tests {
    use super::*;

    /// The AWS SDK family's `Resource` names this service by its SDK id lower-cased — not
    /// by the signing name its requests carry — and an operation whose bindings are
    /// stated is one a call is made for.
    #[test]
    fn the_family_names_this_service_by_its_sdk_id() {
        assert_eq!(
            APIGATEWAYV2_API.sdk_token(),
            "apigatewayv2",
            "the family's token for this service is its SDK id lower-cased, got {:?}",
            APIGATEWAYV2_API.sdk_token()
        );
        assert_ne!(
            APIGATEWAYV2_API.sdk_token(),
            APIGATEWAY_NAMESPACE,
            "this service's token is not the signing name its requests carry"
        );
        // (case, the operation, whether a request to it is bound)
        let rows: [(&str, &str, bool); 4] = [
            ("the listing the recorded workflow calls", GET_APIS_OPERATION, true),
            ("the write of an api", CREATE_API_OPERATION, true),
            ("the read of one", GET_API_OPERATION, true),
            ("an operation of the service with no binding", "GetAuthorizers", false),
        ];
        for (case, operation, expected) in rows {
            let bound = APIGATEWAYV2_REST
                .iter()
                .any(|rest| rest.operation == operation);
            assert_eq!(
                bound, expected,
                "case {case:?}: expected bound={expected}, got {bound}"
            );
        }
    }
}
