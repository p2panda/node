use p2panda_rs::atomic::{Author, Hash, Message, MessageAction, MessageValue, SeqNum};
use sqlx::{query, query_as};

use crate::db::Pool;
use crate::errors::Result;

pub async fn materialize(
    pool: &Pool,
    entry_hash: &Hash,
    seq_num: &SeqNum,
    author: &Author,
    message: &Message,
) -> Result<()> {
    let pool = pool.clone();
    let schema = message.schema();

    // @TODO: Handle case when someone gave us an (invalid) empty message, this actually should
    // have been caught earlier in RPC params validation
    let fields = message.fields().unwrap();

    let table_name = schema.as_hex();

    // @TODO: Get schema fields from database and create SQL query accordingly
    query(&format!(
        "
        CREATE TABLE IF NOT EXISTS \"{}\" (
            id          VARCHAR(132)       NOT NULL,
            author      VARCHAR(132)       NOT NULL,
            message     TEXT               NOT NULL,
            date        VARCHAR(128)       NOT NULL,
            seq_num     BIGINT             NOT NULL,
            PRIMARY KEY (id)
        );
        ",
        table_name
    ))
    .execute(&pool)
    .await?;

    let id = match message.action() {
        MessageAction::Create => entry_hash.as_hex(),
        _ => message.id().unwrap().as_hex(),
    };

    let field_message: &String = match fields.get("message") {
        Some(MessageValue::Text(ref value)) => value,
        None => {
            panic!("Field does not exist!");
        }
        _ => {
            panic!("Unimplemented type");
        }
    };

    let field_date: &String = match fields.get("date") {
        Some(MessageValue::Text(ref value)) => value,
        None => {
            panic!("Field does not exist!");
        }
        _ => {
            panic!("Unimplemented type");
        }
    };

    let result: (SeqNum, i64,) = query_as(
        &format!(
            "
            SELECT seq_num, COUNT(id) as count
            FROM \"{}\"
            WHERE
                id = $1
            ",
            table_name
        )
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;

    // @TODO: Check if sequence number is the next one to materialize
    let is_already_initialized = result.1 == 1;
    if is_already_initialized {
        query(&format!(
            "
            UPDATE \"{}\" SET (
                message,
                date,
                seq_num,
            ) = ($1, $2, $3)
            WHERE
                id = $4
            ",
            table_name
        ))
        .bind(field_message)
        .bind(field_date)
        .bind(seq_num)
        .bind(id)
        .execute(&pool)
        .await?;
    } else {
        query(&format!(
            "
            INSERT INTO
                \"{}\"
            VALUES
                ($1, $2, $3, $4, $5)
            ",
            table_name
        ))
        .bind(id)
        .bind(author)
        .bind(field_message)
        .bind(field_date)
        .bind(seq_num)
        .execute(&pool)
        .await?;
    };

    Ok(())
}
