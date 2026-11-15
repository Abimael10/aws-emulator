// SPDX-License-Identifier: CC0-1.0

//! The configuration a stack carries for the members its create request did not set.
//!
//! Every configuration member of `CreateStack` is optional, but a stack has an EFFECTIVE
//! value for the ones the service configures itself: which actions a user may perform
//! during a streaming session, whether application settings persist between sessions, and
//! the streaming protocol the stack prefers. Those values are what the created resource
//! reports, so they are materialised ONCE here, written into the stored record by the
//! create, and projected from it by the create's own response and by every later read —
//! there is no second place a default could be applied, and no reader that renders one the
//! record does not hold.
//!
//! A member the service configures no value for stays absent: a request member with a
//! documented REQUEST default (the storage-connector authentication a session reuses) is a
//! default for how the request is interpreted, not a value the resource reports back, so
//! nothing is written for it.

use crate::ops::types::{ApplicationSettingsResponse, StreamingExperienceSettings, UserSetting};

/// The `Permission` of an action a user may perform, one of `PERMISSION_VALUES`
/// (`ops/types/shared_types.rs`).
const PERMISSION_ENABLED: &str = "ENABLED";

/// The `Permission` of an action a user may not perform, the other `Permission` value.
const PERMISSION_DISABLED: &str = "DISABLED";

/// The permission a stack reports for each action when its create named no `UserSettings`,
/// in the order the resource reports them: the two clipboard directions, the two
/// file-transfer directions, printing, then the two domain sign-in methods and time-zone
/// redirection.
///
/// Every value of the `Action` enum has an entry, because a stack permits or denies each
/// of them whether or not the create mentioned it. The action names are `Action`'s
/// (`smithy_query(query="shape", shape="Action")`, kept as `ACTION_VALUES` in
/// `ops/types/shared_types.rs`); `Stack.UserSettings` documents the permission of most of
/// them — "The actions that are enabled or disabled for users during their streaming
/// sessions. By default these actions are enabled" — while smart-card sign-in and
/// time-zone redirection are the two the service leaves off until a stack enables them.
pub const DEFAULT_USER_SETTINGS: &[(&str, &str)] = &[
    ("CLIPBOARD_COPY_FROM_LOCAL_DEVICE", PERMISSION_ENABLED),
    ("CLIPBOARD_COPY_TO_LOCAL_DEVICE", PERMISSION_ENABLED),
    ("FILE_UPLOAD", PERMISSION_ENABLED),
    ("FILE_DOWNLOAD", PERMISSION_ENABLED),
    ("PRINTING_TO_LOCAL_DEVICE", PERMISSION_ENABLED),
    ("DOMAIN_PASSWORD_SIGNIN", PERMISSION_ENABLED),
    ("DOMAIN_SMART_CARD_SIGNIN", PERMISSION_DISABLED),
    ("AUTO_TIME_ZONE_REDIRECTION", PERMISSION_DISABLED),
];

/// The `PreferredProtocol` a stack prefers when its create named no
/// `StreamingExperienceSettings`, one of `PREFERRED_PROTOCOL_VALUES`
/// (`ops/types/shared_types.rs`). The other value, UDP, "is only supported in the Windows
/// native client" (`smithy_query(query="shape", shape="StreamingExperienceSettings")`), so
/// the protocol every client can stream over is the one a stack prefers until it asks for
/// the other.
pub const DEFAULT_PREFERRED_PROTOCOL: &str = "TCP";

/// The `UserSettings` of a stack whose create named none: one entry per modelled action,
/// carrying [`DEFAULT_USER_SETTINGS`]'s permission for it.
///
/// No `MaximumLength` is reported: the clipboard limit is documented as a value the
/// request specifies for the two clipboard actions, not one the resource reports when
/// nothing set it (`smithy_query(query="shape", shape="UserSetting")`).
pub fn default_user_settings() -> Vec<UserSetting> {
    DEFAULT_USER_SETTINGS
        .iter()
        .map(|(action, permission)| UserSetting {
            action: (*action).to_string(),
            permission: (*permission).to_string(),
            maximum_length: None,
        })
        .collect()
}

/// The persistent application settings of a stack whose create named none: settings are
/// not kept between sessions, so there is no settings group and no bucket to keep them in.
pub fn default_application_settings() -> ApplicationSettingsResponse {
    ApplicationSettingsResponse {
        enabled: Some(false),
        settings_group: None,
        s3_bucket_name: None,
    }
}

/// The streaming experience of a stack whose create named none: the protocol every client
/// can stream over ([`DEFAULT_PREFERRED_PROTOCOL`]).
pub fn default_streaming_experience_settings() -> StreamingExperienceSettings {
    StreamingExperienceSettings {
        preferred_protocol: Some(DEFAULT_PREFERRED_PROTOCOL.to_string()),
    }
}

/// The lifecycle state a created stack reports.
///
/// A stack is a configuration — the fleet it associates, the user access policies it
/// applies and the storage it connects — with nothing to provision, so `CreateStack`
/// answers with a stack that is already usable and no operation of the service moves it out
/// of that state. It is written onto the stored record by the create and projected from
/// there, like the three configuration members above.
pub const CREATED_STACK_STATE: &str = "ACTIVE";

