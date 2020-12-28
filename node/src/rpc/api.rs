use async_std::channel::{unbounded, Sender};
use jsonrpc_core::{BoxFuture, IoHandler, Params, Result};
use jsonrpc_derive::rpc;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
pub struct EntryArgsRequest {
    author: String,
    schema: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EntryArgsResponse {
    encoded_entry_backlink: Option<String>,
    encoded_entry_skiplink: Option<String>,
    last_seq_num: u64,
    log_id: u64,
}

/// Node RPC API methods.
#[rpc(server)]
pub trait Api {
    #[rpc(name = "panda_getEntryArguments", params = "raw")]
    fn get_entry_args(&self, params: Params) -> BoxFuture<Result<EntryArgsResponse>>;
}

#[derive(Debug)]
enum ApiServiceMessages {
    GetEntryArgs(EntryArgsRequest, Sender<Result<EntryArgsResponse>>),
}

/// Service implementing API methods.
pub struct ApiService {
    service_channel: Sender<ApiServiceMessages>,
}

impl ApiService {
    /// Creates a JSON RPC API service.
    pub fn new() -> Self {
        let (service_channel, service_channel_notifier) = unbounded::<ApiServiceMessages>();

        async_std::task::spawn(async move {
            match service_channel_notifier.recv().await {
                Ok(ApiServiceMessages::GetEntryArgs(_params, back_channel)) => {
                    back_channel
                        .send(Ok(EntryArgsResponse {
                            encoded_entry_backlink: Some(String::from("encoded_entry_backlink")),
                            encoded_entry_skiplink: Some(String::from("skiplink")),
                            last_seq_num: 1,
                            log_id: 0,
                        }))
                        .await
                        .unwrap();
                }
                _ => {}
            }
        });

        ApiService { service_channel }
    }

    /// Creates JSON RPC API service and wraps it around a jsonrpc_core IoHandler object which can
    /// be used for a server exposing the API.
    pub fn io_handler() -> IoHandler {
        let mut io = IoHandler::default();
        io.extend_with(Api::to_delegate(ApiService::new()));
        io
    }
}

impl Api for ApiService {
    fn get_entry_args(&self, params_raw: Params) -> BoxFuture<Result<EntryArgsResponse>> {
        let service_channel = self.service_channel.clone();

        Box::pin(async move {
            let params: EntryArgsRequest = params_raw.parse()?;
            let (back_channel, back_channel_notifier) = unbounded();

            async_std::task::block_on(
                service_channel.send(ApiServiceMessages::GetEntryArgs(params, back_channel)),
            )
            .unwrap();

            async_std::task::block_on(back_channel_notifier.recv()).unwrap()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ApiService;
    use jsonrpc_core::ErrorCode;

    // Helper method to generate valid JSON RPC request string
    fn rpc_request(method: &str, params: &str) -> String {
        format!(
            r#"{{
                "jsonrpc": "2.0",
                "method": "{}",
                "params": {},
                "id": 1
            }}"#,
            method, params
        )
        .replace(" ", "")
        .replace("\n", "")
    }

    // Helper method to generate valid JSON RPC response string
    fn rpc_response(result: &str) -> String {
        format!(
            r#"{{
                "jsonrpc": "2.0",
                "result": {},
                "id": 1
            }}"#,
            result
        )
        .replace(" ", "")
        .replace("\n", "")
    }

    // Helper method to generate valid JSON RPC error response string
    fn rpc_error(code: ErrorCode, message: &str) -> String {
        format!(
            r#"{{
                "jsonrpc": "2.0",
                "error": {{
                    "code": {},
                    "message": "<message>"
                }},
                "id": 1
            }}"#,
            code.code(),
        )
        .replace(" ", "")
        .replace("\n", "")
        .replace("<message>", message)
    }

    #[test]
    fn respond_with_missing_param_error() {
        let io = ApiService::io_handler();

        let request = rpc_request(
            "panda_getEntryArguments",
            r#"{
                "schema": "test"
            }"#,
        );

        let response = rpc_error(
            ErrorCode::InvalidParams,
            "Invalid params: missing field `author`.",
        );

        assert_eq!(io.handle_request_sync(&request), Some(response));
    }

    #[test]
    fn next_entry_arguments() {
        let io = ApiService::io_handler();

        let request = rpc_request(
            "panda_getEntryArguments",
            r#"{
                "author": "world",
                "schema": "test"
            }"#,
        );

        let response = rpc_response(
            r#"{
                "encodedEntryBacklink": "encoded_entry_backlink",
                "encodedEntrySkiplink": "skiplink",
                "lastSeqNum": 1,
                "logId": 0
            }"#,
        );

        assert_eq!(io.handle_request_sync(&request), Some(response));
    }
}
