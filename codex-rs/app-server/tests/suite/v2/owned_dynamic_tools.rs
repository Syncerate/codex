use super::connection_handling_websocket::DEFAULT_READ_TIMEOUT;
use super::connection_handling_websocket::WsClient;
use super::connection_handling_websocket::connect_websocket;
use super::connection_handling_websocket::create_config_toml;
use super::connection_handling_websocket::read_error_for_id;
use super::connection_handling_websocket::read_jsonrpc_message;
use super::connection_handling_websocket::send_jsonrpc;
use super::connection_handling_websocket::send_request;
use super::connection_handling_websocket::spawn_websocket_server;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use core_test_support::responses;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const OWNED_SET: &str = "thread/dynamicTools/owned/set";
const LEGACY_SET: &str = "thread/dynamicTools/set";

fn tool(name: &str) -> Value {
    json!({
        "type": "function", "name": name, "description": "Integration test tool",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
    })
}

fn namespace(name: &str, child: &str) -> Value {
    json!({"type": "namespace", "name": name, "description": "Test namespace", "tools": [tool(child)]})
}

fn set_params(thread: &str, tools: Value) -> Value {
    json!({"threadId": thread, "dynamicTools": tools})
}

fn reclaim_params(thread: &str, tools: Value, tokens: &Value) -> Value {
    json!({"threadId": thread, "dynamicTools": tools, "reconnectTokens": tokens})
}

fn tokens(result: &Value, names: &[&str]) -> Result<Value> {
    let map = result["reconnectTokens"].as_object().context("token map")?;
    assert_eq!(map.len(), names.len());
    for name in names {
        assert!(
            map.get(*name)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        );
    }
    Ok(result["reconnectTokens"].clone())
}

fn turn_params(thread: &str) -> Value {
    json!({"threadId": thread, "input": [{"type": "text", "text": "sample tools"}]})
}

fn call_sse(call_id: &str, group: Option<&str>, name: &str) -> String {
    let mut item =
        json!({"type": "function_call", "call_id": call_id, "name": name, "arguments": "{}"});
    if let Some(group) = group {
        item["namespace"] = json!(group);
    }
    responses::sse(vec![
        responses::ev_response_created(call_id),
        json!({"type": "response.output_item.done", "item": item}),
        responses::ev_completed(call_id),
    ])
}

fn answer(text: &str) -> Value {
    json!({"success": true, "contentItems": [{"type": "inputText", "text": text}]})
}

async fn bodies(server: &MockServer) -> Result<Vec<Value>> {
    server
        .received_requests()
        .await
        .context("mock requests")?
        .into_iter()
        .filter(|r| r.url.path() == "/v1/responses")
        .map(|r| serde_json::from_slice(&r.body).map_err(Into::into))
        .collect()
}

fn assert_catalog(body: &Value, expected: &[&str], absent: &[&str]) -> Result<()> {
    let tools = body["tools"].as_array().context("model tool list")?;
    for name in expected {
        assert!(tools.iter().any(|t| t["name"] == *name), "missing {name}");
    }
    for name in absent {
        assert!(
            !tools.iter().any(|t| t["name"] == *name),
            "unexpected {name}"
        );
    }
    Ok(())
}

async fn initialize(ws: &mut WsClient, experimental: bool) -> Result<()> {
    ws_rpc(
        ws,
        1,
        "initialize",
        json!({
            "clientInfo": {"name": "owned_tools_test", "version": "0.1"},
            "capabilities": {"experimentalApi": experimental}
        }),
    )
    .await?;
    send_jsonrpc(
        ws,
        JSONRPCMessage::Notification(JSONRPCNotification {
            method: "initialized".into(),
            params: None,
        }),
    )
    .await
}

// Unlike the generic response helper, never silently discard a misrouted callback.
// RPC responses also serve as an ordering barrier after spoofed result/error frames.
async fn ws_rpc(ws: &mut WsClient, id: i64, method: &str, params: Value) -> Result<Value> {
    send_request(ws, method, id, Some(params)).await?;
    loop {
        match read_jsonrpc_message(ws).await? {
            JSONRPCMessage::Response(r) if r.id == RequestId::Integer(id) => return Ok(r.result),
            JSONRPCMessage::Error(e) if e.id == RequestId::Integer(id) => {
                bail!("RPC {method} failed: {}", e.error.message)
            }
            JSONRPCMessage::Request(_) => bail!("unexpected callback while awaiting {method}"),
            _ => {}
        }
    }
}

async fn ws_reject(
    ws: &mut WsClient,
    id: i64,
    method: &str,
    params: Value,
) -> Result<JSONRPCError> {
    send_request(ws, method, id, Some(params)).await?;
    let error = read_error_for_id(ws, id).await?;
    assert_eq!(error.error.code, -32600);
    Ok(error)
}

async fn ws_notification(ws: &mut WsClient, method: &str) -> Result<Value> {
    loop {
        match read_jsonrpc_message(ws).await? {
            JSONRPCMessage::Notification(n) if n.method == method => {
                return n.params.context("notification params");
            }
            JSONRPCMessage::Request(_) => bail!("callback leaked to nonowner"),
            _ => {}
        }
    }
}

async fn ws_started(ws: &mut WsClient, call_id: &str) -> Result<Value> {
    loop {
        let params = ws_notification(ws, "item/started").await?;
        if params["item"]["id"] == call_id {
            assert_eq!(params["item"]["type"], "dynamicToolCall");
            return Ok(params);
        }
    }
}

async fn ws_call(ws: &mut WsClient, call_id: &str) -> Result<(RequestId, Value)> {
    let mut request = None;
    let mut started = false;
    while !started || request.is_none() {
        match read_jsonrpc_message(ws).await? {
            JSONRPCMessage::Request(r) => {
                assert_eq!(r.method, "item/tool/call");
                let params = r.params.context("callback params")?;
                assert_eq!(params["callId"], call_id);
                request = Some((r.id, params));
            }
            JSONRPCMessage::Notification(n) if n.method == "item/started" => {
                started |= n.params.is_some_and(|p| p["item"]["id"] == call_id);
            }
            _ => {}
        }
    }
    request.context("owner callback")
}

async fn ws_answer(ws: &mut WsClient, id: RequestId, text: &str) -> Result<()> {
    send_jsonrpc(
        ws,
        JSONRPCMessage::Response(JSONRPCResponse {
            id,
            result: answer(text),
        }),
    )
    .await
}

async fn ws_sample(ws: &mut WsClient, id: i64, thread: &str) -> Result<()> {
    ws_rpc(ws, id, "turn/start", turn_params(thread)).await?;
    ws_notification(ws, "turn/completed").await?;
    Ok(())
}

async fn stdio_rpc(mcp: &mut TestAppServer, method: &str, params: Value) -> Result<Value> {
    let id = mcp.send_raw_request(method, Some(params)).await?;
    Ok(timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(id)),
    )
    .await??
    .result)
}

async fn stdio_reject(mcp: &mut TestAppServer, method: &str, params: Value) -> Result<()> {
    let id = mcp.send_raw_request(method, Some(params)).await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    Ok(())
}

async fn stdio_sample(mcp: &mut TestAppServer, thread: &str) -> Result<()> {
    stdio_rpc(mcp, "turn/start", turn_params(thread)).await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    Ok(())
}

#[tokio::test]
async fn owned_set_scopes_replace_and_clear_and_preserves_loaded_resume() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(
        (0..5)
            .map(|_| create_final_assistant_message_sse_response("Done"))
            .collect::<Result<Vec<_>>>()?,
    )
    .await;
    let home = TempDir::new()?;
    create_config_toml(home.path(), &server.uri(), "never")?;
    let (_process, addr) = spawn_websocket_server(home.path()).await?;
    let mut owner = connect_websocket(addr).await?;
    let mut other = connect_websocket(addr).await?;
    initialize(&mut owner, true).await?;
    initialize(&mut other, true).await?;
    let started = ws_rpc(
        &mut owner,
        2,
        "thread/start",
        json!({"dynamicTools": [tool("legacy")]}),
    )
    .await?;
    let thread = started["thread"]["id"].as_str().context("thread id")?;
    let first = ws_rpc(
        &mut owner,
        3,
        OWNED_SET,
        set_params(thread, json!([namespace("group", "child")])),
    )
    .await?;
    let saved = tokens(&first, &["group"])?;
    // Fresh threads persist their rollout on the first completed turn. Resume
    // needs that durable history even when the runtime is already loaded.
    ws_sample(&mut owner, 90, thread).await?;
    ws_rpc(&mut other, 2, "thread/resume", json!({"threadId": thread})).await?;
    ws_rpc(
        &mut other,
        3,
        OWNED_SET,
        set_params(thread, json!([tool("other")])),
    )
    .await?;
    // Ownership belongs to the namespace, even if another client proposes a different child.
    ws_reject(
        &mut other,
        4,
        OWNED_SET,
        set_params(thread, json!([namespace("group", "different_child")])),
    )
    .await?;
    ws_reject(
        &mut owner,
        4,
        OWNED_SET,
        set_params(thread, json!([tool("legacy")])),
    )
    .await?;
    ws_reject(&mut other, 5, LEGACY_SET, set_params(thread, json!([]))).await?;
    ws_sample(&mut owner, 5, thread).await?;

    // A loaded resume must preserve the runtime catalog and its connection owner.
    ws_rpc(&mut other, 6, "thread/resume", json!({"threadId": thread})).await?;
    ws_reject(
        &mut other,
        7,
        OWNED_SET,
        reclaim_params(thread, json!([namespace("group", "child")]), &saved),
    )
    .await?;
    let again = ws_rpc(
        &mut owner,
        6,
        OWNED_SET,
        set_params(thread, json!([namespace("group", "child")])),
    )
    .await?;
    assert!(tokens(&again, &["group"])? == saved);
    ws_reject(&mut owner, 7, LEGACY_SET, set_params(thread, json!([]))).await?;
    ws_sample(&mut owner, 8, thread).await?;

    ws_rpc(
        &mut owner,
        9,
        OWNED_SET,
        set_params(thread, json!([tool("replacement")])),
    )
    .await?;
    ws_sample(&mut owner, 10, thread).await?;
    let cleared = ws_rpc(&mut owner, 11, OWNED_SET, set_params(thread, json!([]))).await?;
    tokens(&cleared, &[])?;
    ws_sample(&mut owner, 12, thread).await?;
    ws_reject(&mut owner, 13, LEGACY_SET, set_params(thread, json!([]))).await?;
    ws_rpc(&mut other, 8, OWNED_SET, set_params(thread, json!([]))).await?;
    ws_rpc(&mut owner, 14, LEGACY_SET, set_params(thread, json!([]))).await?;
    let requests = bodies(&server).await?;
    assert_eq!(requests.len(), 5);
    assert_catalog(
        &requests[0],
        &["legacy", "group"],
        &["other", "replacement"],
    )?;
    for body in &requests[1..3] {
        assert_catalog(body, &["legacy", "group", "other"], &["replacement"])?;
    }
    assert_catalog(
        &requests[3],
        &["legacy", "replacement", "other"],
        &["group"],
    )?;
    assert_catalog(
        &requests[4],
        &["legacy", "other"],
        &["group", "replacement"],
    )?;
    for body in &requests {
        let serialized = serde_json::to_string(body)?;
        for token in saved.as_object().context("saved tokens")?.values() {
            assert!(!serialized.contains(token.as_str().context("token")?));
        }
    }
    Ok(())
}

#[tokio::test]
async fn owned_callback_is_owner_only_and_spoofs_and_active_set_do_not_mutate_state() -> Result<()>
{
    let server = create_mock_responses_server_sequence_unchecked(vec![
        create_final_assistant_message_sse_response("Durable fixture")?,
        call_sse("owned-call", Some("group"), "child"),
        create_final_assistant_message_sse_response("Done")?,
        create_final_assistant_message_sse_response("Idle")?,
    ])
    .await;
    let home = TempDir::new()?;
    create_config_toml(home.path(), &server.uri(), "never")?;
    let (_process, addr) = spawn_websocket_server(home.path()).await?;
    let mut owner = connect_websocket(addr).await?;
    let mut stock = connect_websocket(addr).await?;
    initialize(&mut owner, true).await?;
    initialize(&mut stock, false).await?;
    let started = ws_rpc(&mut owner, 2, "thread/start", json!({})).await?;
    let thread = started["thread"]["id"].as_str().context("thread id")?;
    let result = ws_rpc(
        &mut owner,
        3,
        OWNED_SET,
        set_params(thread, json!([namespace("group", "child")])),
    )
    .await?;
    let saved = tokens(&result, &["group"])?;
    // Materialize the rollout before the stock client subscribes via resume.
    ws_sample(&mut owner, 90, thread).await?;
    ws_rpc(&mut stock, 2, "thread/resume", json!({"threadId": thread})).await?;
    let experimental_denial = ws_reject(
        &mut stock,
        100,
        OWNED_SET,
        set_params(thread, json!([tool("stock_claim")])),
    )
    .await?;
    assert!(
        experimental_denial
            .error
            .message
            .contains("experimentalApi")
    );
    ws_rpc(&mut owner, 4, "turn/start", turn_params(thread)).await?;
    let (callback_id, params) = ws_call(&mut owner, "owned-call").await?;
    assert_eq!(params["namespace"], "group");
    assert_eq!(params["tool"], "child");
    let started_item = ws_started(&mut stock, "owned-call").await?;
    assert_eq!(started_item["threadId"], thread);
    assert_eq!(started_item["item"]["tool"], "child");
    assert!(
        !serde_json::to_string(&started_item)?.contains(saved["group"].as_str().context("token")?)
    );

    let error = ws_reject(
        &mut owner,
        5,
        OWNED_SET,
        set_params(thread, json!([tool("active_candidate")])),
    )
    .await?;
    assert!(error.error.message.contains("idle thread"));
    let serialized = serde_json::to_string(&error)?;
    assert!(!serialized.contains("reconnectTokens"));
    assert!(!serialized.contains("active_candidate"));
    assert!(!serialized.contains(saved["group"].as_str().context("token")?));

    ws_answer(&mut stock, callback_id.clone(), "spoof-result").await?;
    send_jsonrpc(
        &mut stock,
        JSONRPCMessage::Error(JSONRPCError {
            id: callback_id.clone(),
            error: JSONRPCErrorError {
                code: -32603,
                message: "spoof-error".into(),
                data: None,
            },
        }),
    )
    .await?;
    ws_rpc(&mut stock, 3, "thread/loaded/list", json!({})).await?;
    assert_eq!(bodies(&server).await?.len(), 2);
    // Even a loaded resume while the call is pending must not replay it.
    ws_rpc(&mut stock, 4, "thread/resume", json!({"threadId": thread})).await?;
    ws_rpc(&mut owner, 6, "thread/resume", json!({"threadId": thread})).await?;
    ws_answer(&mut owner, callback_id, "owner-result").await?;
    ws_notification(&mut owner, "turn/completed").await?;
    ws_notification(&mut stock, "turn/completed").await?;
    let history = ws_rpc(
        &mut stock,
        5,
        "thread/read",
        json!({"threadId": thread, "includeTurns": true}),
    )
    .await?;
    assert!(!serde_json::to_string(&history)?.contains(saved["group"].as_str().context("token")?));

    // A fresh connection acquiring the rejected name proves no reservation was committed.
    let mut fresh = connect_websocket(addr).await?;
    initialize(&mut fresh, true).await?;
    ws_rpc(&mut fresh, 2, "thread/resume", json!({"threadId": thread})).await?;
    ws_rpc(
        &mut fresh,
        3,
        OWNED_SET,
        set_params(thread, json!([tool("active_candidate")])),
    )
    .await?;
    ws_sample(&mut owner, 7, thread).await?;
    let requests = bodies(&server).await?;
    assert_eq!(requests.len(), 4);
    assert_catalog(
        &requests[0],
        &["group"],
        &["active_candidate", "stock_claim"],
    )?;
    assert_catalog(
        &requests[3],
        &["group", "active_candidate"],
        &["stock_claim"],
    )?;
    let input = serde_json::to_string(&requests[2]["input"])?;
    assert!(input.contains("owner-result"));
    assert!(!input.contains("spoof-result"));
    assert!(!input.contains("spoof-error"));
    Ok(())
}

#[tokio::test]
async fn disconnect_reconnect_reclaims_only_future_callbacks_without_pending_replay() -> Result<()>
{
    let server = create_mock_responses_server_sequence_unchecked(vec![
        create_final_assistant_message_sse_response("Durable fixture")?,
        call_sse("before-disconnect", None, "owned"),
        create_final_assistant_message_sse_response("Disconnected")?,
        call_sse("after-reconnect", None, "owned"),
        create_final_assistant_message_sse_response("Reconnected")?,
    ])
    .await;
    let home = TempDir::new()?;
    create_config_toml(home.path(), &server.uri(), "never")?;
    let (_process, addr) = spawn_websocket_server(home.path()).await?;
    let mut owner = connect_websocket(addr).await?;
    let mut observer = connect_websocket(addr).await?;
    initialize(&mut owner, true).await?;
    initialize(&mut observer, false).await?;
    let started = ws_rpc(&mut owner, 2, "thread/start", json!({})).await?;
    let thread = started["thread"]["id"].as_str().context("thread id")?;
    let result = ws_rpc(
        &mut owner,
        3,
        OWNED_SET,
        set_params(thread, json!([tool("owned")])),
    )
    .await?;
    let saved = tokens(&result, &["owned"])?;
    // Materialize the rollout before the observer subscribes via resume.
    ws_sample(&mut owner, 90, thread).await?;
    ws_rpc(
        &mut observer,
        2,
        "thread/resume",
        json!({"threadId": thread}),
    )
    .await?;
    ws_rpc(&mut owner, 4, "turn/start", turn_params(thread)).await?;
    let (old_id, _) = ws_call(&mut owner, "before-disconnect").await?;
    ws_started(&mut observer, "before-disconnect").await?;
    owner.close(None).await?;
    drop(owner);
    ws_notification(&mut observer, "turn/completed").await?;

    let mut replacement = connect_websocket(addr).await?;
    initialize(&mut replacement, true).await?;
    ws_rpc(
        &mut replacement,
        2,
        "thread/resume",
        json!({"threadId": thread}),
    )
    .await?;
    ws_reject(
        &mut replacement,
        3,
        OWNED_SET,
        set_params(thread, json!([tool("owned")])),
    )
    .await?;
    ws_reject(
        &mut replacement,
        4,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &json!({"owned": "wrong"})),
    )
    .await?;
    let reclaimed = ws_rpc(
        &mut replacement,
        5,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &saved),
    )
    .await?;
    assert!(tokens(&reclaimed, &["owned"])? == saved);
    // Reusing the old callback id cannot resurrect a cancelled invocation.
    ws_answer(&mut replacement, old_id.clone(), "stale-result").await?;
    ws_rpc(&mut replacement, 6, "thread/loaded/list", json!({})).await?;
    ws_rpc(&mut replacement, 7, "turn/start", turn_params(thread)).await?;
    let (new_id, _) = ws_call(&mut replacement, "after-reconnect").await?;
    assert_ne!(new_id, old_id);
    ws_started(&mut observer, "after-reconnect").await?;
    ws_answer(&mut replacement, new_id, "future-result").await?;
    ws_notification(&mut replacement, "turn/completed").await?;
    ws_notification(&mut observer, "turn/completed").await?;
    let requests = bodies(&server).await?;
    assert_eq!(requests.len(), 5);
    assert_catalog(&requests[0], &["owned"], &[])?;
    let input = serde_json::to_string(&requests[4]["input"])?;
    assert!(input.contains("future-result"));
    assert!(!input.contains("stale-result"));
    Ok(())
}

#[tokio::test]
async fn unload_resume_requires_explicit_reclaim_and_clear_retires_tokens() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(
        (0..3)
            .map(|_| create_final_assistant_message_sse_response("Done"))
            .collect::<Result<Vec<_>>>()?,
    )
    .await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_root_config("thread_unload_delay_secs = 0")
        .write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let started = stdio_rpc(
        &mut mcp,
        "thread/start",
        json!({"dynamicTools": [tool("legacy")]}),
    )
    .await?;
    let thread = started["thread"]["id"].as_str().context("thread id")?;
    let result = stdio_rpc(
        &mut mcp,
        OWNED_SET,
        set_params(thread, json!([tool("owned")])),
    )
    .await?;
    let saved = tokens(&result, &["owned"])?;
    stdio_sample(&mut mcp, thread).await?;
    // Unsubscribe the only client and wait for the native idle-unload notification.
    stdio_rpc(&mut mcp, "thread/unsubscribe", json!({"threadId": thread})).await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/closed"),
    )
    .await??;
    let loaded = stdio_rpc(&mut mcp, "thread/loaded/list", json!({})).await?;
    assert_eq!(loaded["data"], json!([]));
    stdio_rpc(&mut mcp, "thread/resume", json!({"threadId": thread})).await?;
    stdio_reject(&mut mcp, LEGACY_SET, set_params(thread, json!([]))).await?;
    stdio_reject(
        &mut mcp,
        OWNED_SET,
        set_params(thread, json!([tool("owned")])),
    )
    .await?;
    stdio_reject(
        &mut mcp,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &json!({"owned": "wrong"})),
    )
    .await?;
    stdio_sample(&mut mcp, thread).await?;
    let result = stdio_rpc(
        &mut mcp,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &saved),
    )
    .await?;
    assert!(tokens(&result, &["owned"])? == saved);
    stdio_sample(&mut mcp, thread).await?;
    stdio_rpc(&mut mcp, OWNED_SET, set_params(thread, json!([]))).await?;
    stdio_reject(
        &mut mcp,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &saved),
    )
    .await?;
    let new = stdio_rpc(
        &mut mcp,
        OWNED_SET,
        set_params(thread, json!([tool("owned")])),
    )
    .await?;
    assert!(tokens(&new, &["owned"])? != saved);
    stdio_reject(
        &mut mcp,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &saved),
    )
    .await?;
    let requests = bodies(&server).await?;
    assert_eq!(requests.len(), 3);
    assert_catalog(&requests[0], &["legacy", "owned"], &[])?;
    assert_catalog(&requests[1], &["legacy"], &["owned"])?;
    assert_catalog(&requests[2], &["legacy", "owned"], &[])?;
    Ok(())
}

#[tokio::test]
async fn fork_does_not_inherit_runtime_tools_or_parent_reconnect_capabilities() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(
        (0..4)
            .map(|_| create_final_assistant_message_sse_response("Done"))
            .collect::<Result<Vec<_>>>()?,
    )
    .await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let started = stdio_rpc(
        &mut mcp,
        "thread/start",
        json!({"dynamicTools": [tool("legacy")]}),
    )
    .await?;
    let parent = started["thread"]["id"].as_str().context("parent id")?;
    let result = stdio_rpc(
        &mut mcp,
        OWNED_SET,
        set_params(parent, json!([tool("owned")])),
    )
    .await?;
    let saved = tokens(&result, &["owned"])?;
    stdio_sample(&mut mcp, parent).await?;
    let forked = stdio_rpc(&mut mcp, "thread/fork", json!({"threadId": parent})).await?;
    let child = forked["thread"]["id"].as_str().context("fork id")?;
    assert_ne!(child, parent);
    stdio_reject(
        &mut mcp,
        OWNED_SET,
        reclaim_params(child, json!([tool("owned")]), &saved),
    )
    .await?;
    stdio_sample(&mut mcp, child).await?;
    // No inherited reservation may block the legacy setter in the child.
    stdio_rpc(
        &mut mcp,
        LEGACY_SET,
        set_params(child, json!([tool("legacy")])),
    )
    .await?;
    let child_result = stdio_rpc(
        &mut mcp,
        OWNED_SET,
        set_params(child, json!([tool("owned")])),
    )
    .await?;
    assert!(tokens(&child_result, &["owned"])? != saved);
    stdio_sample(&mut mcp, child).await?;
    stdio_reject(&mut mcp, LEGACY_SET, set_params(parent, json!([]))).await?;
    stdio_sample(&mut mcp, parent).await?;
    let requests = bodies(&server).await?;
    assert_eq!(requests.len(), 4);
    assert_catalog(&requests[0], &["legacy", "owned"], &[])?;
    assert_catalog(&requests[1], &["legacy"], &["owned"])?;
    assert_catalog(&requests[2], &["legacy", "owned"], &[])?;
    assert_catalog(&requests[3], &["legacy", "owned"], &[])?;
    Ok(())
}

#[tokio::test]
async fn process_restart_mints_fresh_ownership_without_replaying_pending_calls() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(vec![
        create_final_assistant_message_sse_response("Durable restart fixture")?,
        call_sse("restart-pending", None, "owned"),
        create_final_assistant_message_sse_response("Restored startup catalog")?,
        call_sse("restart-future", None, "owned"),
        create_final_assistant_message_sse_response("Fresh owner answered")?,
        create_final_assistant_message_sse_response("Cleared catalog")?,
    ])
    .await;
    let home = TempDir::new()?;
    create_config_toml(home.path(), &server.uri(), "never")?;
    let (mut first_process, first_addr) = spawn_websocket_server(home.path()).await?;
    let first_pid = first_process.id().context("first process pid")?;
    let mut original = connect_websocket(first_addr).await?;
    initialize(&mut original, true).await?;
    let started = ws_rpc(
        &mut original,
        2,
        "thread/start",
        json!({"dynamicTools": [tool("legacy")]}),
    )
    .await?;
    let thread = started["thread"]["id"].as_str().context("thread id")?;
    let registered = ws_rpc(
        &mut original,
        3,
        OWNED_SET,
        set_params(thread, json!([tool("owned")])),
    )
    .await?;
    let old_tokens = tokens(&registered, &["owned"])?;
    ws_sample(&mut original, 4, thread).await?;
    let before_history = ws_rpc(
        &mut original,
        5,
        "thread/read",
        json!({"threadId": thread, "includeTurns": true}),
    )
    .await?;
    let rollout_path = before_history["thread"]["path"]
        .as_str()
        .context("durable rollout path")?
        .to_owned();

    // Kill the real process with an outstanding effect. Closing the socket first
    // would cancel the callback and exercise disconnect instead of crash recovery.
    ws_rpc(&mut original, 6, "turn/start", turn_params(thread)).await?;
    let (_, pending_params) = ws_call(&mut original, "restart-pending").await?;
    assert_eq!(pending_params["threadId"], thread);
    assert_eq!(bodies(&server).await?.len(), 2);
    timeout(DEFAULT_READ_TIMEOUT, first_process.kill()).await??;
    timeout(DEFAULT_READ_TIMEOUT, first_process.wait()).await??;
    drop(original);

    // Keep the same home, rollout, and mock server, but replace the app-server
    // process and every connection. The old capability is now unknown here.
    let (mut second_process, second_addr) = spawn_websocket_server(home.path()).await?;
    assert_ne!(
        second_process.id().context("second process pid")?,
        first_pid
    );
    let mut current = connect_websocket(second_addr).await?;
    let mut stock = connect_websocket(second_addr).await?;
    initialize(&mut current, true).await?;
    initialize(&mut stock, false).await?;
    let resumed = ws_rpc(
        &mut current,
        2,
        "thread/resume",
        json!({"threadId": thread}),
    )
    .await?;
    assert_eq!(resumed["thread"]["id"], thread);
    ws_rpc(&mut stock, 2, "thread/resume", json!({"threadId": thread})).await?;
    // These reads reject any callback: neither resume nor a fresh turn may
    // replay the invocation left pending in the terminated process.
    ws_sample(&mut current, 3, thread).await?;
    ws_notification(&mut stock, "turn/completed").await?;
    let registered = ws_rpc(
        &mut current,
        4,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &old_tokens),
    )
    .await?;
    let replacement_tokens = tokens(&registered, &["owned"])?;
    assert!(replacement_tokens != old_tokens);

    ws_rpc(&mut current, 5, "turn/start", turn_params(thread)).await?;
    let (callback_id, callback_params) = ws_call(&mut current, "restart-future").await?;
    assert_eq!(callback_params["threadId"], thread);
    assert_eq!(callback_params["tool"], "owned");
    let started_item = ws_started(&mut stock, "restart-future").await?;
    ws_answer(&mut current, callback_id, "fresh-process-result").await?;
    ws_notification(&mut current, "turn/completed").await?;
    ws_notification(&mut stock, "turn/completed").await?;

    let cleared = ws_rpc(&mut current, 6, OWNED_SET, set_params(thread, json!([]))).await?;
    tokens(&cleared, &[])?;
    // Both the consumed prior-process token and the freshly issued token are
    // known in this process. Clearing retires them; neither may add the name.
    let old_denial = ws_reject(
        &mut current,
        7,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &old_tokens),
    )
    .await?;
    let replacement_denial = ws_reject(
        &mut current,
        8,
        OWNED_SET,
        reclaim_params(thread, json!([tool("owned")]), &replacement_tokens),
    )
    .await?;
    ws_sample(&mut current, 9, thread).await?;
    ws_notification(&mut stock, "turn/completed").await?;
    let after_history = ws_rpc(
        &mut stock,
        3,
        "thread/read",
        json!({"threadId": thread, "includeTurns": true}),
    )
    .await?;
    let requests = bodies(&server).await?;
    assert_eq!(requests.len(), 6);
    for body in &requests[..2] {
        assert_catalog(body, &["legacy", "owned"], &[])?;
    }
    assert_catalog(&requests[2], &["legacy"], &["owned"])?;
    for body in &requests[3..5] {
        assert_catalog(body, &["legacy", "owned"], &[])?;
    }
    assert_catalog(&requests[5], &["legacy"], &["owned"])?;
    let outputs = requests[4]["input"]
        .as_array()
        .context("continuation input")?;
    assert!(outputs.iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "restart-future"
            && item.to_string().contains("fresh-process-result")
    }));
    let rollout = std::fs::read_to_string(&rollout_path)?;
    for saved in [&old_tokens, &replacement_tokens] {
        let token = saved["owned"].as_str().context("saved capability")?;
        assert!(!rollout.contains(token));
        for value in [
            &before_history,
            &resumed,
            &after_history,
            &pending_params,
            &callback_params,
            &started_item,
        ] {
            assert!(!serde_json::to_string(value)?.contains(token));
        }
        for error in [&old_denial, &replacement_denial] {
            assert!(!serde_json::to_string(error)?.contains(token));
        }
        for body in &requests {
            assert!(!serde_json::to_string(body)?.contains(token));
        }
    }
    timeout(DEFAULT_READ_TIMEOUT, second_process.kill()).await??;
    timeout(DEFAULT_READ_TIMEOUT, second_process.wait()).await??;
    Ok(())
}
