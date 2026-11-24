// SPDX-License-Identifier: CC0-1.0

//! Whether the resources a bucket-policy statement targets are resources this service names
//! at all.
//!
//! A statement's `Resource` element names the resources its grant covers, and a resource is
//! named by its ARN: Amazon S3 publishes `arn:<partition>:s3:::<bucket>` for a general purpose
//! bucket, `arn:<partition>:s3:::<bucket>/<key>` for an object in one, and its own forms for
//! the resources addressed through an access point or an Outposts bucket. A value that is no
//! ARN of this service — a bare bucket name, a key, a resource of another service — names no
//! resource a bucket policy could grant on, so the statement grants nothing and S3 refuses the
//! document rather than storing it.
//!
//! The values are the ones the shared reading of that element reports
//! (`::service_common::iam_policy::resource_element_values`, which walks the same statements
//! the policy grammar does and covers `NotResource` too); what this module adds is the
//! service's own half of the question — the ARN has to be one of ITS resources. The element
//! that covers every resource (`::service_common::iam_policy::ANY_RESOURCE`) covers this
//! service's among them, and WHICH bucket's resources a well-formed ARN names is not this
//! reading's question: an action that applies to none of them is
//! [`super::policy_action_fault`]'s.

#[allow(unused_imports)]
use super::*;

/// How S3 words the `MalformedPolicy` of a document whose `Resource` names no resource of
/// this service — fixed text stating the condition and nothing of the request.
pub const INVALID_RESOURCE_MESSAGE: &str = "Policy has invalid resource";

/// The first resource value `policy`'s statements name that is no resource of this service,
/// or `None` when every one of them is.
///
/// The values are read in the order the document writes them, so the one reported is the
/// first a reader of the document would reach, and it is reported as submitted. A text that
/// is no policy names no value, so it carries no fault here: what makes a document unusable
/// as a policy at all is [`super::is_valid_policy_document`]'s reading, taken before this
/// one.
#[must_use]
pub fn policy_resource_fault(policy: &str) -> Option<String> {
    ::service_common::iam_policy::resource_element_values(policy)
        .into_iter()
        .find(|value| !names_a_resource(value))
}

/// Whether `resource` names a resource of this service: the element covering every resource,
/// or an ARN whose vendor field is this service's (see the module docs).
#[must_use]
pub fn names_a_resource(resource: &str) -> bool {
    resource == ::service_common::iam_policy::ANY_RESOURCE
        || ::service_common::arn::parse(resource).is_some_and(|arn| arn.vendor == S3_VENDOR)
}

/// The ARN vendor field of an Amazon S3 resource.
const S3_VENDOR: &str = "s3";

#[cfg(test)]
mod tests {
    use super::*;

    /// Which values name a resource of this service, over the forms a document writes.
    #[test]
    fn a_resource_is_named_by_an_arn_of_this_service() {
        let rows: &[(&str, bool)] = &[
            ("*", true),
            ("arn:aws:s3:::reports", true),
            ("arn:aws:s3:::reports/*", true),
            ("arn:aws:s3:::reports/quarter/1", true),
            ("arn:aws-us-gov:s3:::reports", true),
            ("arn:aws:s3:::*", true),
            // Another bucket's resources are still this service's: WHICH bucket a grant
            // covers is not a fault of the document's form.
            ("arn:aws:s3:::another-bucket/*", true),
            ("arn:aws:s3:us-east-1:000000000000:accesspoint/reports", true),
            // A bare name, a key and another service's resource name no S3 resource.
            ("reports", false),
            ("reports/*", false),
            ("arn:aws:sqs:us-east-1:000000000000:reports", false),
            ("arn:aws:s3", false),
            ("", false),
        ];
        for (resource, named) in rows {
            assert_eq!(
                names_a_resource(resource),
                *named,
                "case {resource:?}: expected named={named}, got {}",
                names_a_resource(resource)
            );
        }
    }

    /// The document reading reports the first such value, over the forms the element and the
    /// statements are written in.
    #[test]
    fn the_first_value_naming_no_resource_is_reported() {
        let rows: &[(&str, Option<&str>)] = &[
            (
                r#"{"Statement":[{"Sid":"Read","Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::reports/*"}]}"#,
                None,
            ),
            (
                r#"{"Statement":[{"Sid":"Read","Effect":"Allow","Action":"s3:GetObject","Resource":["arn:aws:s3:::reports/*","reports/private"]}]}"#,
                Some("reports/private"),
            ),
            // The statement written as the object itself, and the complement element.
            (
                r#"{"Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"reports/*"}}"#,
                Some("reports/*"),
            ),
            (
                r#"{"Statement":[{"Effect":"Deny","Action":"s3:GetObject","NotResource":"arn:aws:sqs:us-east-1:000000000000:q"}]}"#,
                Some("arn:aws:sqs:us-east-1:000000000000:q"),
            ),
            // A statement naming no resource has none to be wrong about.
            (
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:GetObject"}]}"#,
                None,
            ),
        ];
        for (document, expected) in rows {
            let reported = policy_resource_fault(document);
            assert_eq!(
                reported.as_deref(),
                *expected,
                "case {document:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }
}
