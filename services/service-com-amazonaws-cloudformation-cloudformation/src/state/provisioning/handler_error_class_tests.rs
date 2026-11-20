// SPDX-License-Identifier: CC0-1.0

//! The unit tests of [`super`]'s `HandlerErrorCode` classification, in a file of their own: this
//! module's own file is at the length a change to this repository may touch.

use super::{
    handler_error_code, sdk_attempt_count, HANDLER_ERROR_CLASSES, HANDLER_ERROR_INVALID_REQUEST,
    HANDLER_ERROR_SERVICE_FAILED, SDK_RETRIED_ATTEMPTS, SDK_RETRIED_CLASSES,
    SDK_SINGLE_ATTEMPT,
};
use crate::ops::types::HANDLER_ERROR_CODE_VALUES;

/// Every class the SDK diagnostic counts attempts for is one the classification above
/// can actually answer, so the retried set cannot drift from the class table.
#[test]
fn every_retried_class_is_one_the_classification_answers() {
    for class in SDK_RETRIED_CLASSES {
        assert!(
            HANDLER_ERROR_CLASSES.iter().any(|(named, _)| *named == class),
            "case {class:?}: not a class the refusal classification answers, which are \
                 {HANDLER_ERROR_CLASSES:?}"
        );
    }
}

/// The attempt count follows the CLASS of the refusal: the SDK's default maximum for a
/// condition that can clear on its own, one attempt for a refusal of the request itself.
#[test]
fn the_attempt_count_follows_the_class() {
    for (code, expected) in [
        ("LimitExceededException", SDK_RETRIED_ATTEMPTS),
        ("ThrottlingException", SDK_RETRIED_ATTEMPTS),
        ("ServiceUnavailable", SDK_RETRIED_ATTEMPTS),
        ("HTTP 503", SDK_RETRIED_ATTEMPTS),
        ("ValidationException", SDK_SINGLE_ATTEMPT),
        ("InvalidTypeException", SDK_SINGLE_ATTEMPT),
        ("ResourceNotFoundException", SDK_SINGLE_ATTEMPT),
        ("HTTP 400", SDK_SINGLE_ATTEMPT),
    ] {
        let actual = sdk_attempt_count(code);
        assert_eq!(
            actual, expected,
            "case {code:?} ({}): expected {expected} attempt(s), got {actual}",
            handler_error_code(code)
        );
    }
}

/// Every class this module reports is one the MODEL declares: the classification is a
/// mapping onto `HandlerErrorCode`, so a class spelled any other way would put a value
/// on the wire that enum does not have.
#[test]
fn every_reported_class_is_one_the_model_declares() {
    for class in HANDLER_ERROR_CLASSES
        .iter()
        .map(|(class, _)| *class)
        .chain([HANDLER_ERROR_INVALID_REQUEST, HANDLER_ERROR_SERVICE_FAILED])
    {
        assert!(
            HANDLER_ERROR_CODE_VALUES.contains(&class),
            "case {class:?}: not a HandlerErrorCode the model declares, which are \
                 {HANDLER_ERROR_CODE_VALUES:?}"
        );
    }
}

/// And so is whatever the classification answers for an arbitrary code, including one
/// no class matches and an answer that named no error at all.
#[test]
fn every_answer_is_one_the_model_declares() {
    for code in [
        "DBSubnetGroupNotFoundFault",
        "InvalidParameterValue",
        "ThrottlingException",
        "BucketAlreadyOwnedByYou",
        "ResourceInUseException",
        "LimitExceededException",
        "AccessDeniedException",
        "a-code-no-service-answers",
        "",
        "HTTP 501",
        "HTTP 400",
        "HTTP not-a-status",
    ] {
        let class = handler_error_code(code);
        assert!(
            HANDLER_ERROR_CODE_VALUES.contains(&class),
            "case {code:?}: answered {class:?}, which is not a HandlerErrorCode the \
                 model declares"
        );
    }
}
