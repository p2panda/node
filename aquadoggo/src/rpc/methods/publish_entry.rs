use std::convert::TryFrom;

use jsonrpc_v2::{Data, Params};
use p2panda_rs::atomic::{Entry as EntryUnsigned, Message, Validation};

use crate::db::models::{Entry, Log};
use crate::errors::Result;
use crate::materializer::materialize;
use crate::rpc::request::PublishEntryRequest;
use crate::rpc::response::PublishEntryResponse;
use crate::rpc::RpcApiState;

#[derive(thiserror::Error, Debug)]
#[allow(missing_copy_implementations)]
pub enum PublishEntryError {
    #[error("Could not find backlink entry in database")]
    BacklinkMissing,

    #[error("Could not find skiplink entry in database")]
    SkiplinkMissing,

    #[error("Claimed log_id for schema not the same as in database")]
    InvalidLogId,
}

/// Implementation of `panda_publishEntry` RPC method.
///
/// Stores an author's Bamboo entry with message payload in database after validating it.
pub async fn publish_entry(
    data: Data<RpcApiState>,
    Params(params): Params<PublishEntryRequest>,
) -> Result<PublishEntryResponse> {
    // Validate request parameters
    params.entry_encoded.validate()?;
    params.message_encoded.validate()?;

    // Get database connection pool
    let pool = data.pool.clone();

    // Handle error as this conversion validates message hash
    let entry = EntryUnsigned::try_from((&params.entry_encoded, Some(&params.message_encoded)))?;
    let message = Message::from(&params.message_encoded);

    // Retreive author and schema
    let author = params.entry_encoded.author();
    let schema = message.schema();

    // Determine log_id for author's schema
    let schema_log_id = Log::get(&pool, &author, &schema).await?;

    // Check if log_id is the same as the previously claimed one (when given)
    if schema_log_id.is_some() && schema_log_id.as_ref() != Some(entry.log_id()) {
        Err(PublishEntryError::InvalidLogId)?;
    }

    // Get related bamboo backlink and skiplink entries
    let entry_backlink_bytes = if !entry.seq_num().is_first() {
        Entry::at_seq_num(
            &pool,
            &author,
            &entry.log_id(),
            &entry.seq_num_backlink().unwrap(),
        )
        .await?
        .map(|link| {
            Some(
                hex::decode(link.entry_bytes)
                    .expect("Backlink entry with invalid hex-encoding detected in database"),
            )
        })
        .ok_or(PublishEntryError::BacklinkMissing)
    } else {
        Ok(None)
    }?;

    let entry_skiplink_bytes = if !entry.seq_num().is_first() {
        Entry::at_seq_num(
            &pool,
            &author,
            &entry.log_id(),
            &entry.seq_num_skiplink().unwrap(),
        )
        .await?
        .map(|link| {
            Some(
                hex::decode(link.entry_bytes)
                    .expect("Skiplink entry with invalid hex-encoding detected in database"),
            )
        })
        .ok_or(PublishEntryError::SkiplinkMissing)
    } else {
        Ok(None)
    }?;

    // Verify bamboo entry integrity
    bamboo_rs_core::verify(
        &params.entry_encoded.to_bytes(),
        Some(&params.message_encoded.to_bytes()),
        entry_skiplink_bytes.as_deref(),
        entry_backlink_bytes.as_deref(),
    )?;

    // Register used log id in database when not set yet
    if schema_log_id.is_none() {
        Log::insert(&pool, &author, &schema, entry.log_id()).await?;
    }

    // Finally insert Entry in database
    Entry::insert(
        &pool,
        &author,
        &params.entry_encoded,
        &params.entry_encoded.hash(),
        &entry.log_id(),
        &params.message_encoded,
        &params.message_encoded.hash(),
        &entry.seq_num(),
    )
    .await?;

    // Start materialization
    materialize(
        &pool,
        &params.entry_encoded.hash(),
        &entry.seq_num(),
        &author,
        &message,
    )
    .await?;

    // Already return arguments for next entry creation
    let entry_latest = Entry::latest(&pool, &author, &entry.log_id())
        .await?
        .expect("Database does not contain any entries");
    let entry_hash_skiplink = super::entry_args::determine_skiplink(pool, &entry_latest).await?;

    Ok(PublishEntryResponse {
        entry_hash_backlink: Some(params.entry_encoded.hash()),
        entry_hash_skiplink,
        last_seq_num: Some(entry.seq_num().to_owned()),
        log_id: entry.log_id().to_owned(),
    })
}
#[cfg(test)]
mod tests {
    use std::convert::TryFrom;

    use p2panda_rs::atomic::{
        Entry, EntrySigned, Hash, LogId, Message, MessageEncoded, MessageFields, MessageValue,
        SeqNum,
    };
    use p2panda_rs::key_pair::KeyPair;

    use crate::rpc::api::build_rpc_api_service;
    use crate::rpc::server::{build_rpc_server, RpcServer};
    use crate::test_helpers::{handle_http, initialize_db, rpc_error, rpc_request, rpc_response};

    // Helper method to create encoded entries and messages
    fn create_test_entry(
        key_pair: &KeyPair,
        schema: &Hash,
        log_id: &LogId,
        skiplink: Option<&EntrySigned>,
        backlink: Option<&EntrySigned>,
        previous_seq_num: Option<&SeqNum>,
    ) -> (EntrySigned, MessageEncoded) {
        // Create message with dummy data
        let mut fields = MessageFields::new();
        fields
            .add("test", MessageValue::Text("Hello".to_owned()))
            .unwrap();
        let message = Message::new_create(schema.clone(), fields).unwrap();

        // Encode message
        let message_encoded = MessageEncoded::try_from(&message).unwrap();

        // Create, sign and encode entry
        let entry = Entry::new(
            log_id,
            &message,
            skiplink.map(|e| e.hash()).as_ref(),
            backlink.map(|e| e.hash()).as_ref(),
            previous_seq_num,
        )
        .unwrap();
        let entry_encoded = EntrySigned::try_from((&entry, key_pair)).unwrap();

        (entry_encoded, message_encoded)
    }

    // Helper method to compare expected API responses with what was returned
    async fn assert_request(
        app: &RpcServer,
        entry_encoded: &EntrySigned,
        message_encoded: &MessageEncoded,
        entry_skiplink: Option<&EntrySigned>,
        log_id: &LogId,
        seq_num: &SeqNum,
    ) {
        // Prepare request to API
        let request = rpc_request(
            "panda_publishEntry",
            &format!(
                r#"{{
                    "entryEncoded": "{}",
                    "messageEncoded": "{}"
                }}"#,
                entry_encoded.as_str(),
                message_encoded.as_str(),
            ),
        );

        // Prepare expected response result
        let skiplink_str = match entry_skiplink {
            Some(entry) => {
                format!("\"{}\"", entry.hash().as_hex())
            }
            None => "null".to_owned(),
        };

        let response = rpc_response(&format!(
            r#"{{
                "entryHashBacklink": "{}",
                "entryHashSkiplink": {},
                "lastSeqNum": {},
                "logId": {}
            }}"#,
            entry_encoded.hash().as_hex(),
            skiplink_str,
            seq_num.as_i64(),
            log_id.as_i64(),
        ));

        assert_eq!(handle_http(&app, request).await, response);
    }

    #[async_std::test]
    async fn publish_entry() {
        // Create key pair for author
        let key_pair = KeyPair::new();

        // Prepare test database
        let pool = initialize_db().await;

        // Create tide server with endpoints
        let rpc_api = build_rpc_api_service(pool);
        let app = build_rpc_server(rpc_api);

        // Define schema and log id for entries
        let schema = Hash::new_from_bytes(vec![1, 2, 3]).unwrap();
        let log_id = LogId::new(5);

        // Create a couple of entries in the same log and check for consistency
        //
        // [1] --
        let (entry_1, message_1) = create_test_entry(&key_pair, &schema, &log_id, None, None, None);
        assert_request(
            &app,
            &entry_1,
            &message_1,
            None,
            &log_id,
            &SeqNum::new(1).unwrap(),
        )
        .await;

        // [1] <-- [2]
        let (entry_2, message_2) = create_test_entry(
            &key_pair,
            &schema,
            &log_id,
            None,
            Some(&entry_1),
            Some(&SeqNum::new(1).unwrap()),
        );
        assert_request(
            &app,
            &entry_2,
            &message_2,
            None,
            &log_id,
            &SeqNum::new(2).unwrap(),
        )
        .await;

        // [1] <-- [2] <-- [3]
        let (entry_3, message_3) = create_test_entry(
            &key_pair,
            &schema,
            &log_id,
            None,
            Some(&entry_2),
            Some(&SeqNum::new(2).unwrap()),
        );
        assert_request(
            &app,
            &entry_3,
            &message_3,
            Some(&entry_1),
            &log_id,
            &SeqNum::new(3).unwrap(),
        )
        .await;

        //  /------------------ [4]
        // [1] <-- [2] <-- [3]
        let (entry_4, message_4) = create_test_entry(
            &key_pair,
            &schema,
            &log_id,
            Some(&entry_1),
            Some(&entry_3),
            Some(&SeqNum::new(3).unwrap()),
        );
        assert_request(
            &app,
            &entry_4,
            &message_4,
            None,
            &log_id,
            &SeqNum::new(4).unwrap(),
        )
        .await;

        //  /------------------ [4]
        // [1] <-- [2] <-- [3]   \-- [5] --
        let (entry_5, message_5) = create_test_entry(
            &key_pair,
            &schema,
            &log_id,
            None,
            Some(&entry_4),
            Some(&SeqNum::new(4).unwrap()),
        );
        assert_request(
            &app,
            &entry_5,
            &message_5,
            None,
            &log_id,
            &SeqNum::new(5).unwrap(),
        )
        .await;
    }

    #[async_std::test]
    async fn validate() {
        // Create key pair for author
        let key_pair = KeyPair::new();

        // Prepare test database
        let pool = initialize_db().await;

        // Create tide server with endpoints
        let rpc_api = build_rpc_api_service(pool);
        let app = build_rpc_server(rpc_api);

        // Define schema and log id for entries
        let schema = Hash::new_from_bytes(vec![1, 2, 3]).unwrap();
        let log_id = LogId::new(5);

        // Create two valid entries for testing
        let (entry_1, message_1) = create_test_entry(&key_pair, &schema, &log_id, None, None, None);
        assert_request(
            &app,
            &entry_1,
            &message_1,
            None,
            &log_id,
            &SeqNum::new(1).unwrap(),
        )
        .await;

        let (entry_2, message_2) = create_test_entry(
            &key_pair,
            &schema,
            &log_id,
            None,
            Some(&entry_1),
            Some(&SeqNum::new(1).unwrap()),
        );
        assert_request(
            &app,
            &entry_2,
            &message_2,
            None,
            &log_id,
            &SeqNum::new(2).unwrap(),
        )
        .await;

        // Send invalid log id for this schema
        let (entry_wrong_log_id, _) = create_test_entry(
            &key_pair,
            &schema,
            &LogId::new(1),
            None,
            Some(&entry_1),
            Some(&SeqNum::new(1).unwrap()),
        );

        let request = rpc_request(
            "panda_publishEntry",
            &format!(
                r#"{{
                    "entryEncoded": "{}",
                    "messageEncoded": "{}"
                }}"#,
                entry_wrong_log_id.as_str(),
                message_2.as_str(),
            ),
        );

        let response = rpc_error("Claimed log_id for schema not the same as in database");

        assert_eq!(handle_http(&app, request).await, response);

        // Send invalid backlink entry / hash
        let (entry_wrong_hash, _) = create_test_entry(
            &key_pair,
            &schema,
            &log_id,
            Some(&entry_2),
            Some(&entry_1),
            Some(&SeqNum::new(2).unwrap()),
        );

        let request = rpc_request(
            "panda_publishEntry",
            &format!(
                r#"{{
                    "entryEncoded": "{}",
                    "messageEncoded": "{}"
                }}"#,
                entry_wrong_hash.as_str(),
                message_2.as_str(),
            ),
        );

        let response = rpc_error(
            "The backlink hash encoded in the entry does not match the lipmaa entry provided",
        );

        assert_eq!(handle_http(&app, request).await, response);

        // Send invalid seq num
        let (entry_wrong_seq_num, _) = create_test_entry(
            &key_pair,
            &schema,
            &log_id,
            None,
            Some(&entry_2),
            Some(&SeqNum::new(5).unwrap()),
        );

        let request = rpc_request(
            "panda_publishEntry",
            &format!(
                r#"{{
                    "entryEncoded": "{}",
                    "messageEncoded": "{}"
                }}"#,
                entry_wrong_seq_num.as_str(),
                message_2.as_str(),
            ),
        );

        let response = rpc_error("Could not find backlink entry in database");

        assert_eq!(handle_http(&app, request).await, response);
    }
}
