// SPDX-License-Identifier: CC0-1.0

//! Amazon CloudFront's own wire facts, which this crate reaches for the reason Amazon S3's
//! are reached: a caller names an operation of it through the AWS SDK integration FAMILY,
//! and nothing else.
//!
//! CloudFront publishes no optimized integration of its own, so
//! `arn:aws:states:::cloudfront:<action>` names no `Task` state's work — only the family's
//! own `aws-sdk:cloudfront:<operation>` does, which is also how Amazon EventBridge
//! Scheduler's UNIVERSAL target names a call
//! ([`super::aws_sdk_call`]). Its entry is therefore kept out of [`super::APIS`], the
//! table the optimized family is looked up in, and lives here with the REST bindings of
//! the operations those callers make, so the one concern sits in one file.
//!
//! Its protocol is `restXml`: each operation binds its members to a method, a path, the
//! query and one payload exactly as a REST-JSON service does, and the payload member is
//! an XML DOCUMENT rather than a JSON one ([`super::Body::XmlMember`],
//! [`super::xml_body`]).

use super::*;

/// Amazon CloudFront, for the calls this crate makes to it: a `Task` state whose
/// `Resource` names one of its operations through the AWS SDK integration family, and an
/// EventBridge Scheduler universal target that names one the same way.
///
/// `services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/
/// com-amazonaws-cloudfront-cloudfront2020-05-31.json`
/// (`smithy_query(query="service_info")`): restXml, sigv4Name `cloudfront`,
/// endpointPrefix `cloudfront`, arnNamespace `cloudfront`, sdkId `CloudFront`,
/// xmlNamespace `http://cloudfront.amazonaws.com/doc/2020-05-31/`.
pub(super) static CLOUDFRONT_API: Api = Api {
    namespace: CLOUDFRONT_NAMESPACE,
    action_prefix: None,
    signing_name: CLOUDFRONT_NAMESPACE,
    endpoint_prefix: CLOUDFRONT_NAMESPACE,
    wire: Wire::Rest(CLOUDFRONT_REST),
    sdk_id: "CloudFront",
    error_prefix: "CloudFront",
    // The AWS SDK names the service `CloudFront`, which its own `CloudFront` client
    // carries.
    sdk_error_prefix: "CloudFront",
    // The client class `AmazonCloudFrontClient`, whose base exception is
    // `AmazonCloudFrontException`.
    sdk_client: "AmazonCloudFront",
    sdk_exception: "AmazonCloudFrontException",
    // Every operation of this service models `AccessDenied` (HTTP 403) for a call its
    // caller may not make, which is also how its front door answers one
    // (`smithy_query(query="operation",operation="CreateInvalidation")`: `AccessDenied`,
    // httpError 403).
    access_denied_status: TARGET_FORBIDDEN_STATUS,
    access_denied_code: ACCESS_DENIED_SHAPE,
    lowercases_refused_action: false,
    // `CreateInvalidation`'s input: `DistributionId`, `InvalidationBatch`.
    member_case: MemberCase::Pascal,
    query_compatible: false,
    models_errors: true,
};

/// CloudFront's ARN namespace, its signing name and its endpoint prefix, which are the
/// same label.
pub(super) const CLOUDFRONT_NAMESPACE: &str = "cloudfront";

/// The error shape every operation of this service declares for a call its caller is not
/// authorized to make — "Access denied", HTTP 403 — which is both the code its answer
/// names that class by and the shape the SDK deserializes it into
/// ([`Api::access_denied_code`], [`Denied::exception`]).
const ACCESS_DENIED_SHAPE: &str = "AccessDenied";

/// The XML namespace this service's documents are written in: the `xmlNamespace` its
/// model declares, which a request's payload element carries.
const XML_NAMESPACE: &str = "http://cloudfront.amazonaws.com/doc/2020-05-31/";

/// The CloudFront operation that submits an invalidation, as that model spells it, and
/// the two reads of one.
pub(super) const CREATE_INVALIDATION_OPERATION: &str = "CreateInvalidation";
/// See [`CREATE_INVALIDATION_OPERATION`].
const GET_INVALIDATION_OPERATION: &str = "GetInvalidation";
/// See [`CREATE_INVALIDATION_OPERATION`].
const LIST_INVALIDATIONS_OPERATION: &str = "ListInvalidations";

/// The member of a batch and of a listing that carries the list's entries, as both of
/// those shapes spell it.
const ITEMS_MEMBER: &str = "Items";

/// The list members of an invalidation's own documents, each with the element its entries
/// are written under.
///
/// `smithy_query(query="shape",shape="PathList",model_path="services/
/// service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/
/// com-amazonaws-cloudfront-cloudfront2020-05-31.json")`: the list's member carries
/// `xmlName` `Path`, so `Paths$Items` is one `<Path>` element per path.
const INVALIDATION_LISTS: Lists = &[(ITEMS_MEMBER, "Path")];

/// The same for a LISTING of invalidations: `InvalidationSummaryList`'s member carries
/// `xmlName` `InvalidationSummary` (`query="shape",shape="InvalidationSummaryList"` of
/// that same model), so `InvalidationList$Items` is one `<InvalidationSummary>` element
/// per invalidation.
const LISTING_LISTS: Lists = &[(ITEMS_MEMBER, "InvalidationSummary")];

/// The REST bindings of the CloudFront operations this crate calls: the invalidation an
/// AWS SDK integration (or a universal target) submits, and the two reads of one.
///
/// Read from that service's own model with
/// `smithy_query(query="operation",operation="CreateInvalidation,GetInvalidation,
/// ListInvalidations",model_path="services/
/// service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/
/// com-amazonaws-cloudfront-cloudfront2020-05-31.json")`. Each addresses its distribution
/// by a `DistributionId` `httpLabel`, and each binds ONE member as its `httpPayload` —
/// which this protocol carries as an XML document — rather than carrying the operation's
/// members as the body: `CreateInvalidation`'s request is its `InvalidationBatch` and its
/// answer the `Invalidation`, `GetInvalidation`'s answer the same, and
/// `ListInvalidations`'s answer the `InvalidationList`. An operation of this service with
/// no entry is one no call is made for ([`super::read`]).
pub(super) const CLOUDFRONT_REST: &[Rest] = &[
    // `http` POST `/2020-05-31/distribution/{DistributionId}/invalidation`, code 201,
    // with `InvalidationBatch` the request's `httpPayload`, `Invalidation` the answer's,
    // and `Location` the answer's `Location` header.
    Rest {
        operation: CREATE_INVALIDATION_OPERATION,
        method: "POST",
        uri: "/2020-05-31/distribution/{DistributionId}/invalidation",
        request_body: Body::XmlMember {
            member: "InvalidationBatch",
            namespace: XML_NAMESPACE,
            lists: INVALIDATION_LISTS,
        },
        headers: &[],
        query: &[],
        response_body: Body::XmlMember {
            member: "Invalidation",
            namespace: XML_NAMESPACE,
            lists: INVALIDATION_LISTS,
        },
        response_status: "",
        response_headers: &[("Location", "Location")],
    },
    // `http` GET `/2020-05-31/distribution/{DistributionId}/invalidation/{Id}`: both
    // members are `httpLabel`s, so the read sends no body.
    Rest {
        operation: GET_INVALIDATION_OPERATION,
        method: "GET",
        uri: "/2020-05-31/distribution/{DistributionId}/invalidation/{Id}",
        request_body: Body::Empty,
        headers: &[],
        query: &[],
        response_body: Body::XmlMember {
            member: "Invalidation",
            namespace: XML_NAMESPACE,
            lists: INVALIDATION_LISTS,
        },
        response_status: "",
        response_headers: &[],
    },
    // `http` GET `/2020-05-31/distribution/{DistributionId}/invalidation`, with `Marker`
    // and `MaxItems` `httpQuery` members, so the listing sends no body either.
    Rest {
        operation: LIST_INVALIDATIONS_OPERATION,
        method: "GET",
        uri: "/2020-05-31/distribution/{DistributionId}/invalidation",
        request_body: Body::Empty,
        headers: &[],
        query: &[("Marker", "Marker"), ("MaxItems", "MaxItems")],
        response_body: Body::XmlMember {
            member: "InvalidationList",
            namespace: XML_NAMESPACE,
            lists: LISTING_LISTS,
        },
        response_status: "",
        response_headers: &[],
    },
];

/// A CloudFront DISTRIBUTION, named by the `DistributionId` each of these operations
/// takes.
///
/// CloudFront is a GLOBAL service: a distribution's ARN carries no Region label —
/// "arn:aws:cloudfront::123456789012:distribution/EDFDVBD6EXAMPLE" (`Distribution$ARN`
/// of that model) — so the resource an access request over one is taken on is the same
/// ARN whichever Region the call is made in ([`TargetNaming::Global`]).
pub(super) const CLOUDFRONT_DISTRIBUTION: TargetNaming = TargetNaming::Global {
    service: CLOUDFRONT_NAMESPACE,
    prefix: "distribution/",
};

/// The member of each of these operations' requests that names the distribution the call
/// acts on, as that model spells it.
pub(super) const DISTRIBUTION_ID_MEMBER: &str = "DistributionId";

#[cfg(test)]
mod tests {
    use super::*;

    /// Each operation this crate calls has its REST binding stated, and every other
    /// operation of the service is one no call is made for.
    #[test]
    fn the_invalidation_operations_are_the_ones_bound() {
        // (case, the operation, whether a request to it is bound)
        let rows: [(&str, &str, bool); 5] = [
            ("the invalidation a call submits", CREATE_INVALIDATION_OPERATION, true),
            ("the read of one", GET_INVALIDATION_OPERATION, true),
            ("the listing of them", LIST_INVALIDATIONS_OPERATION, true),
            ("an operation of the service with no binding", "CreateDistribution", false),
            ("a name no operation of the service has", "NotAnOperation", false),
        ];
        for (case, operation, expected) in rows {
            let bound = CLOUDFRONT_REST
                .iter()
                .any(|rest| rest.operation == operation);
            assert_eq!(
                bound, expected,
                "case {case:?}: expected bound={expected}, got {bound}"
            );
        }
    }

    /// A distribution's ARN carries no Region, whichever Region the call naming it is
    /// made in.
    #[test]
    fn a_distributions_arn_carries_no_region() {
        for region in ["us-east-1", "eu-west-2"] {
            let arn = CLOUDFRONT_DISTRIBUTION.arn_of("E1EXAMPLE", region);
            assert_eq!(
                arn.as_deref(),
                Some(
                    format!(
                        "arn:aws:cloudfront::{}:distribution/E1EXAMPLE",
                        ::engine::emulator_account_id()
                    )
                    .as_str()
                ),
                "the ARN of a distribution named in {region} carries no Region, got {arn:?}"
            );
        }
    }
}
