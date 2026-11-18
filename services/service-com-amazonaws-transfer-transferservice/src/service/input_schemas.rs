// SPDX-License-Identifier: CC0-1.0

//! Every operation INPUT shape of this service, as the schemas codegen derives from the
//! Smithy model.
//!
//! What they are read for: a constraint the model declares on the ELEMENTS of a collection
//! member is stated at that member with the element shape's WHOLE declared set, and that set
//! is a property of the model rather than of one request — a refusal carries only the
//! constraints a value actually broke. Codegen attaches a collection member's element
//! constraints to that member's schema, which is where the shared rendering
//! (`service_common::validation_message`) reads them, and where the validating walk read them
//! to refuse the request in the first place.
//!
//! ALL of them, not the ones one refusal happens to name: the rule holds for every
//! constrained collection of every operation, and the refusal seam is handed no operation.
//! `grep -h 'super::types::.*_INPUT_SCHEMA' src/ops/*.rs` names exactly these, one per
//! operation file; one missing from this list is not a wrong clause, only a clause stating
//! the constraint that failed rather than the element shape's whole set.

use aws_smithy_schema::Schema;

/// The input shapes a refusal's element sets are read off. See the module header.
pub(crate) const INPUT_SCHEMAS: &[&Schema] = {
    use crate::ops::types;

    &[
        types::CREATE_ACCESS_INPUT_SCHEMA,
        types::CREATE_AGREEMENT_INPUT_SCHEMA,
        types::CREATE_CONNECTOR_INPUT_SCHEMA,
        types::CREATE_PROFILE_INPUT_SCHEMA,
        types::CREATE_SERVER_INPUT_SCHEMA,
        types::CREATE_USER_INPUT_SCHEMA,
        types::CREATE_WEB_APP_INPUT_SCHEMA,
        types::CREATE_WORKFLOW_INPUT_SCHEMA,
        types::DELETE_ACCESS_INPUT_SCHEMA,
        types::DELETE_AGREEMENT_INPUT_SCHEMA,
        types::DELETE_CERTIFICATE_INPUT_SCHEMA,
        types::DELETE_CONNECTOR_INPUT_SCHEMA,
        types::DELETE_HOST_KEY_INPUT_SCHEMA,
        types::DELETE_PROFILE_INPUT_SCHEMA,
        types::DELETE_SERVER_INPUT_SCHEMA,
        types::DELETE_SSH_PUBLIC_KEY_INPUT_SCHEMA,
        types::DELETE_USER_INPUT_SCHEMA,
        types::DELETE_WEB_APP_CUSTOMIZATION_INPUT_SCHEMA,
        types::DELETE_WEB_APP_INPUT_SCHEMA,
        types::DELETE_WORKFLOW_INPUT_SCHEMA,
        types::DESCRIBE_ACCESS_INPUT_SCHEMA,
        types::DESCRIBE_AGREEMENT_INPUT_SCHEMA,
        types::DESCRIBE_CERTIFICATE_INPUT_SCHEMA,
        types::DESCRIBE_CONNECTOR_INPUT_SCHEMA,
        types::DESCRIBE_EXECUTION_INPUT_SCHEMA,
        types::DESCRIBE_HOST_KEY_INPUT_SCHEMA,
        types::DESCRIBE_PROFILE_INPUT_SCHEMA,
        types::DESCRIBE_SECURITY_POLICY_INPUT_SCHEMA,
        types::DESCRIBE_SERVER_INPUT_SCHEMA,
        types::DESCRIBE_USER_INPUT_SCHEMA,
        types::DESCRIBE_WEB_APP_CUSTOMIZATION_INPUT_SCHEMA,
        types::DESCRIBE_WEB_APP_INPUT_SCHEMA,
        types::DESCRIBE_WORKFLOW_INPUT_SCHEMA,
        types::IMPORT_CERTIFICATE_INPUT_SCHEMA,
        types::IMPORT_HOST_KEY_INPUT_SCHEMA,
        types::IMPORT_SSH_PUBLIC_KEY_INPUT_SCHEMA,
        types::LIST_ACCESSES_INPUT_SCHEMA,
        types::LIST_AGREEMENTS_INPUT_SCHEMA,
        types::LIST_CERTIFICATES_INPUT_SCHEMA,
        types::LIST_CONNECTORS_INPUT_SCHEMA,
        types::LIST_EXECUTIONS_INPUT_SCHEMA,
        types::LIST_FILE_TRANSFER_RESULTS_INPUT_SCHEMA,
        types::LIST_HOST_KEYS_INPUT_SCHEMA,
        types::LIST_PROFILES_INPUT_SCHEMA,
        types::LIST_SECURITY_POLICIES_INPUT_SCHEMA,
        types::LIST_SERVERS_INPUT_SCHEMA,
        types::LIST_TAGS_FOR_RESOURCE_INPUT_SCHEMA,
        types::LIST_USERS_INPUT_SCHEMA,
        types::LIST_WEB_APPS_INPUT_SCHEMA,
        types::LIST_WORKFLOWS_INPUT_SCHEMA,
        types::SEND_WORKFLOW_STEP_STATE_INPUT_SCHEMA,
        types::START_DIRECTORY_LISTING_INPUT_SCHEMA,
        types::START_FILE_TRANSFER_INPUT_SCHEMA,
        types::START_REMOTE_DELETE_INPUT_SCHEMA,
        types::START_REMOTE_MOVE_INPUT_SCHEMA,
        types::START_SERVER_INPUT_SCHEMA,
        types::STOP_SERVER_INPUT_SCHEMA,
        types::TAG_RESOURCE_INPUT_SCHEMA,
        types::TEST_CONNECTION_INPUT_SCHEMA,
        types::TEST_IDENTITY_PROVIDER_INPUT_SCHEMA,
        types::UNTAG_RESOURCE_INPUT_SCHEMA,
        types::UPDATE_ACCESS_INPUT_SCHEMA,
        types::UPDATE_AGREEMENT_INPUT_SCHEMA,
        types::UPDATE_CERTIFICATE_INPUT_SCHEMA,
        types::UPDATE_CONNECTOR_INPUT_SCHEMA,
        types::UPDATE_HOST_KEY_INPUT_SCHEMA,
        types::UPDATE_PROFILE_INPUT_SCHEMA,
        types::UPDATE_SERVER_INPUT_SCHEMA,
        types::UPDATE_USER_INPUT_SCHEMA,
        types::UPDATE_WEB_APP_CUSTOMIZATION_INPUT_SCHEMA,
        types::UPDATE_WEB_APP_INPUT_SCHEMA,
    ]
};
