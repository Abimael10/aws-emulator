// SPDX-License-Identifier: CC0-1.0

//! Folding the engine settings a request supplied onto the ones an endpoint holds.
//!
//! `ModifyEndpoint$ExactSettings` publishes both readings of a settings bag a modify
//! carries: "If this attribute is Y, the current call to ModifyEndpoint replaces all
//! existing endpoint settings with the exact settings that you specify in this call. If
//! this attribute is N, the current call to ModifyEndpoint does two things: It replaces
//! any endpoint settings that already exist with new values, for settings with the same
//! names. It creates new endpoint settings that you specify in the call, for settings
//! with different names."
//!
//! So a bag the request carries is folded onto the stored bag SETTING BY SETTING by
//! default — a setting the request names takes the request's value, a setting it does not
//! name keeps the endpoint's — and replaces it wholesale when the request asks for exact
//! settings. A bag the request does not carry at all names no setting for that engine, so
//! it leaves the endpoint's settings for that engine as they were: specifying settings is
//! how a call reaches them, and `ExactSettings` reads "the exact settings that you specify
//! in this call", so a call that specifies none for an engine says nothing about it.

use super::EngineSettings;

/// Take each setting `stored` holds that `supplied` does not name.
///
/// Every member listed is one the model makes optional, so "the request did not name it"
/// is exactly `None`: a member the request named — including one it named explicitly —
/// keeps the value it carried. A member the model makes REQUIRED is not listed, because
/// the request cannot omit it.
macro_rules! fill_unnamed {
    ($supplied:expr, $stored:expr, $($member:ident),+ $(,)?) => {{
        let mut merged = $supplied;
        let stored = $stored;
        $(
            if merged.$member.is_none() {
                merged.$member = stored.$member.clone();
            }
        )+
        merged
    }};
}

/// One engine's settings bag, after the request's bag is folded onto the endpoint's.
///
/// `merge` folds a supplied bag onto a stored one member by member; it is never called
/// when the request asked for exact settings, which is what makes the supplied bag stand
/// alone.
fn folded<T: Clone>(
    supplied: Option<T>,
    stored: &Option<T>,
    exact: bool,
    merge: impl FnOnce(T, &T) -> T,
) -> Option<T> {
    match (supplied, stored) {
        (Some(supplied), Some(stored)) if !exact => Some(merge(supplied, stored)),
        (Some(supplied), _) => Some(supplied),
        (None, stored) => stored.clone(),
    }
}

impl EngineSettings {
    /// These settings — the ones a `ModifyEndpoint` request carried — folded onto the
    /// ones the endpoint holds, with `exact` asking for the request's bags to stand on
    /// their own ([`ModifyEndpoint$ExactSettings`], quoted in this module's docs).
    ///
    /// Call it on the SANITIZED settings a request carried, so no password a modify sent
    /// can reach the store through the fold.
    ///
    /// [`ModifyEndpoint$ExactSettings`]: crate::ops::modify_endpoint
    pub fn folded_onto(self, stored: &EngineSettings, exact: bool) -> EngineSettings {
        EngineSettings {
            // `DynamoDbSettings` models one member, and it is required, so a supplied bag
            // names every setting there is to name.
            dynamo_db: folded(self.dynamo_db, &stored.dynamo_db, exact, |supplied, _| {
                supplied
            }),
            s3: folded(self.s3, &stored.s3, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    service_access_role_arn,
                    external_table_definition,
                    csv_row_delimiter,
                    csv_delimiter,
                    bucket_folder,
                    bucket_name,
                    compression_type,
                    encryption_mode,
                    server_side_encryption_kms_key_id,
                    data_format,
                    encoding_type,
                    dict_page_size_limit,
                    row_group_length,
                    data_page_size,
                    parquet_version,
                    enable_statistics,
                    include_op_for_full_load,
                    cdc_inserts_only,
                    timestamp_column_name,
                    parquet_timestamp_in_millisecond,
                    cdc_inserts_and_updates,
                    date_partition_enabled,
                    date_partition_sequence,
                    date_partition_delimiter,
                    use_csv_no_sup_value,
                    csv_no_sup_value,
                    preserve_transactions,
                    cdc_path,
                    use_task_start_time_for_full_load_timestamp,
                    canned_acl_for_objects,
                    add_column_name,
                    cdc_max_batch_interval,
                    cdc_min_file_size,
                    csv_null_value,
                    ignore_header_rows,
                    max_file_size,
                    rfc4180,
                    date_partition_timezone,
                    add_trailing_padding_character,
                    expected_bucket_owner,
                    glue_catalog_generation,
                )
            }),
            dms_transfer: folded(
                self.dms_transfer,
                &stored.dms_transfer,
                exact,
                |supplied, stored| {
                    fill_unnamed!(supplied, stored, service_access_role_arn, bucket_name)
                },
            ),
            mongo_db: folded(
                self.mongo_db,
                &stored.mongo_db,
                exact,
                |supplied, stored| {
                    fill_unnamed!(
                        supplied,
                        stored,
                        username,
                        password,
                        server_name,
                        port,
                        database_name,
                        auth_type,
                        auth_mechanism,
                        nesting_level,
                        extract_doc_id,
                        docs_to_investigate,
                        auth_source,
                        kms_key_id,
                        secrets_manager_access_role_arn,
                        secrets_manager_secret_id,
                        use_update_look_up,
                        replicate_shard_collections,
                    )
                },
            ),
            kinesis: folded(self.kinesis, &stored.kinesis, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    stream_arn,
                    message_format,
                    service_access_role_arn,
                    include_transaction_details,
                    include_partition_value,
                    partition_include_schema_table,
                    include_table_alter_operations,
                    include_control_details,
                    include_null_and_empty,
                    no_hex_prefix,
                    use_large_integer_value,
                )
            }),
            kafka: folded(self.kafka, &stored.kafka, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    broker,
                    topic,
                    message_format,
                    include_transaction_details,
                    include_partition_value,
                    partition_include_schema_table,
                    include_table_alter_operations,
                    include_control_details,
                    message_max_bytes,
                    include_null_and_empty,
                    security_protocol,
                    ssl_client_certificate_arn,
                    ssl_client_key_arn,
                    ssl_client_key_password,
                    ssl_ca_certificate_arn,
                    sasl_username,
                    sasl_password,
                    no_hex_prefix,
                    sasl_mechanism,
                    ssl_endpoint_identification_algorithm,
                    use_large_integer_value,
                )
            }),
            elasticsearch: folded(
                self.elasticsearch,
                &stored.elasticsearch,
                exact,
                |supplied, stored| {
                    fill_unnamed!(
                        supplied,
                        stored,
                        full_load_error_percentage,
                        error_retry_duration,
                        use_new_mapping_type,
                    )
                },
            ),
            neptune: folded(self.neptune, &stored.neptune, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    service_access_role_arn,
                    error_retry_duration,
                    max_file_size,
                    max_retry_count,
                    iam_auth_enabled,
                )
            }),
            redshift: folded(
                self.redshift,
                &stored.redshift,
                exact,
                |supplied, stored| {
                    fill_unnamed!(
                        supplied,
                        stored,
                        accept_any_date,
                        after_connect_script,
                        bucket_folder,
                        bucket_name,
                        case_sensitive_names,
                        comp_update,
                        connection_timeout,
                        database_name,
                        date_format,
                        empty_as_null,
                        encryption_mode,
                        explicit_ids,
                        file_transfer_upload_streams,
                        load_timeout,
                        max_file_size,
                        password,
                        port,
                        remove_quotes,
                        replace_invalid_chars,
                        replace_chars,
                        server_name,
                        service_access_role_arn,
                        server_side_encryption_kms_key_id,
                        time_format,
                        trim_blanks,
                        truncate_columns,
                        username,
                        write_buffer_size,
                        secrets_manager_access_role_arn,
                        secrets_manager_secret_id,
                        map_boolean_as_boolean,
                    )
                },
            ),
            postgre_sql: folded(
                self.postgre_sql,
                &stored.postgre_sql,
                exact,
                |supplied, stored| {
                    fill_unnamed!(
                        supplied,
                        stored,
                        after_connect_script,
                        capture_ddls,
                        max_file_size,
                        database_name,
                        ddl_artifacts_schema,
                        execute_timeout,
                        fail_tasks_on_lob_truncation,
                        heartbeat_enable,
                        heartbeat_schema,
                        heartbeat_frequency,
                        password,
                        port,
                        server_name,
                        username,
                        slot_name,
                        plugin_name,
                        secrets_manager_access_role_arn,
                        secrets_manager_secret_id,
                        trim_space_in_char,
                        map_boolean_as_boolean,
                        map_jsonb_as_clob,
                        map_long_varchar_as,
                        database_mode,
                        babelfish_database_name,
                        disable_unicode_source_filter,
                        service_access_role_arn,
                        authentication_method,
                    )
                },
            ),
            my_sql: folded(self.my_sql, &stored.my_sql, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    after_connect_script,
                    clean_source_metadata_on_mismatch,
                    database_name,
                    events_poll_interval,
                    target_db_type,
                    max_file_size,
                    parallel_load_threads,
                    password,
                    port,
                    server_name,
                    server_timezone,
                    username,
                    secrets_manager_access_role_arn,
                    secrets_manager_secret_id,
                    execute_timeout,
                    service_access_role_arn,
                    authentication_method,
                )
            }),
            oracle: folded(self.oracle, &stored.oracle, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    add_supplemental_logging,
                    archived_log_dest_id,
                    additional_archived_log_dest_id,
                    extra_archived_log_dest_ids,
                    allow_select_nested_tables,
                    parallel_asm_read_threads,
                    read_ahead_blocks,
                    access_alternate_directly,
                    use_alternate_folder_for_online,
                    oracle_path_prefix,
                    use_path_prefix,
                    replace_path_prefix,
                    enable_homogenous_tablespace,
                    direct_path_no_log,
                    archived_logs_only,
                    asm_password,
                    asm_server,
                    asm_user,
                    char_length_semantics,
                    database_name,
                    direct_path_parallel_load,
                    fail_tasks_on_lob_truncation,
                    number_datatype_scale,
                    password,
                    port,
                    read_table_space_name,
                    retry_interval,
                    security_db_encryption,
                    security_db_encryption_name,
                    server_name,
                    spatial_data_option_to_geo_json_function_name,
                    standby_delay_time,
                    username,
                    use_b_file,
                    use_direct_path_full_load,
                    use_logminer_reader,
                    secrets_manager_access_role_arn,
                    secrets_manager_secret_id,
                    secrets_manager_oracle_asm_access_role_arn,
                    secrets_manager_oracle_asm_secret_id,
                    trim_space_in_char,
                    convert_timestamp_with_zone_to_utc,
                    open_transaction_window,
                    authentication_method,
                )
            }),
            sybase: folded(self.sybase, &stored.sybase, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    database_name,
                    password,
                    port,
                    server_name,
                    username,
                    secrets_manager_access_role_arn,
                    secrets_manager_secret_id,
                )
            }),
            microsoft_sql_server: folded(
                self.microsoft_sql_server,
                &stored.microsoft_sql_server,
                exact,
                |supplied, stored| {
                    fill_unnamed!(
                        supplied,
                        stored,
                        port,
                        bcp_packet_size,
                        database_name,
                        control_tables_file_group,
                        password,
                        query_single_always_on_node,
                        read_backup_only,
                        safeguard_policy,
                        server_name,
                        username,
                        use_bcp_full_load,
                        use_third_party_backup_device,
                        secrets_manager_access_role_arn,
                        secrets_manager_secret_id,
                        trim_space_in_char,
                        tlog_access_mode,
                        force_lob_lookup,
                        authentication_method,
                    )
                },
            ),
            ibm_db2: folded(self.ibm_db2, &stored.ibm_db2, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    database_name,
                    password,
                    port,
                    server_name,
                    set_data_capture_changes,
                    current_lsn,
                    max_k_bytes_per_read,
                    username,
                    secrets_manager_access_role_arn,
                    secrets_manager_secret_id,
                    load_timeout,
                    write_buffer_size,
                    max_file_size,
                    keep_csv_files,
                )
            }),
            doc_db: folded(self.doc_db, &stored.doc_db, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    username,
                    password,
                    server_name,
                    port,
                    database_name,
                    nesting_level,
                    extract_doc_id,
                    docs_to_investigate,
                    kms_key_id,
                    secrets_manager_access_role_arn,
                    secrets_manager_secret_id,
                    use_update_look_up,
                    replicate_shard_collections,
                )
            }),
            redis: folded(self.redis, &stored.redis, exact, |supplied, stored| {
                fill_unnamed!(
                    supplied,
                    stored,
                    ssl_security_protocol,
                    auth_type,
                    auth_user_name,
                    auth_password,
                    ssl_ca_certificate_arn,
                )
            }),
            gcp_my_sql: folded(
                self.gcp_my_sql,
                &stored.gcp_my_sql,
                exact,
                |supplied, stored| {
                    fill_unnamed!(
                        supplied,
                        stored,
                        after_connect_script,
                        clean_source_metadata_on_mismatch,
                        database_name,
                        events_poll_interval,
                        target_db_type,
                        max_file_size,
                        parallel_load_threads,
                        password,
                        port,
                        server_name,
                        server_timezone,
                        username,
                        secrets_manager_access_role_arn,
                        secrets_manager_secret_id,
                    )
                },
            ),
            timestream: folded(
                self.timestream,
                &stored.timestream,
                exact,
                |supplied, stored| {
                    fill_unnamed!(
                        supplied,
                        stored,
                        cdc_inserts_and_updates,
                        enable_magnetic_store_writes,
                    )
                },
            ),
        }
    }
}
