use super::*;
use codex_app_server_protocol::DynamicToolCallParams;
use codex_app_server_protocol::DynamicToolFunctionSpec;
use codex_app_server_protocol::DynamicToolSpec;
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(1);

fn setup() -> (Arc<OutgoingMessageSender>, mpsc::Receiver<OutgoingEnvelope>) {
    let (tx, rx) = mpsc::channel(16);
    (
        Arc::new(OutgoingMessageSender::new(
            tx,
            AnalyticsEventsClient::disabled(),
        )),
        rx,
    )
}

fn tool() -> DynamicToolSpec {
    DynamicToolSpec::Function(DynamicToolFunctionSpec {
        name: "owned_tool".into(),
        description: "Routing test tool".into(),
        input_schema: json!({"type": "object"}),
        defer_loading: false,
    })
}

fn call(thread_id: ThreadId) -> ServerRequestPayload {
    ServerRequestPayload::DynamicToolCall(DynamicToolCallParams {
        thread_id: thread_id.to_string(),
        turn_id: "turn-1".into(),
        call_id: "call-1".into(),
        namespace: None,
        tool: "owned_tool".into(),
        arguments: json!({"input": "test"}),
    })
}

fn answer() -> Result {
    json!({"contentItems": [], "success": true})
}

async fn register(
    outgoing: &OutgoingMessageSender,
    thread_id: ThreadId,
) -> (Vec<DynamicToolSpec>, HashMap<String, String>) {
    let mut ownership = outgoing.dynamic_tool_ownership.lock().await;
    let update = ownership
        .plan(
            thread_id,
            ConnectionId(1),
            &[],
            vec![tool()],
            HashMap::new(),
        )
        .expect("register owner");
    let tools = update.tools.clone();
    let tokens = update.tokens.clone();
    ownership.commit(thread_id, update);
    (tools, tokens)
}

async fn targeted_request(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
    owner: ConnectionId,
    id: &RequestId,
) {
    let envelope = timeout(WAIT, rx.recv())
        .await
        .expect("request should be queued promptly")
        .expect("outgoing queue should remain open");
    let OutgoingEnvelope::ToConnection {
        connection_id,
        message: OutgoingMessage::Request(request),
        ..
    } = envelope
    else {
        panic!("expected a targeted request");
    };
    assert_eq!(connection_id, owner);
    assert_eq!(request.id(), id);
    assert!(matches!(request, ServerRequest::DynamicToolCall { .. }));
}

fn no_message(rx: &mut mpsc::Receiver<OutgoingEnvelope>) {
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

fn pending(waiter: &mut oneshot::Receiver<ClientRequestResult>) {
    assert!(matches!(
        waiter.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
}

async fn closed(waiter: oneshot::Receiver<ClientRequestResult>) {
    assert!(
        timeout(WAIT, waiter)
            .await
            .expect("callback should fail promptly")
            .is_err()
    );
}

#[tokio::test]
async fn owned_call_targets_only_owner_and_ignores_spoofed_result_and_error() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    register(&outgoing, thread_id).await;
    let scoped = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(1), ConnectionId(2)],
        thread_id,
    );
    let (id, mut waiter) = scoped.send_request(call(thread_id)).await;
    targeted_request(&mut rx, ConnectionId(1), &id).await;
    no_message(&mut rx);

    outgoing
        .notify_client_response(ConnectionId(2), id.clone(), answer())
        .await;
    pending(&mut waiter);
    outgoing
        .notify_client_error(ConnectionId(2), id.clone(), internal_error("spoof"))
        .await;
    pending(&mut waiter);
    assert!(
        outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&id)
    );
    assert!(
        outgoing
            .pending_requests_for_thread(thread_id)
            .await
            .is_empty()
    );
    // Even the original owner must not replay an owned invocation.
    for connection in [ConnectionId(1), ConnectionId(2), ConnectionId(3)] {
        outgoing
            .replay_requests_to_connection_for_thread(connection, thread_id)
            .await;
        no_message(&mut rx);
    }

    outgoing
        .notify_client_response(ConnectionId(1), id.clone(), answer())
        .await;
    assert_eq!(
        timeout(WAIT, waiter)
            .await
            .expect("owner response should resolve")
            .expect("callback should deliver"),
        Ok(answer())
    );
    assert!(
        !outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&id)
    );
}

#[tokio::test]
async fn disconnect_cancels_pending_call_and_reconnect_only_receives_future_calls() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    let (tools, tokens) = register(&outgoing, thread_id).await;
    let scoped = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(1), ConnectionId(2)],
        thread_id,
    );
    let (old_id, old_waiter) = scoped.send_request(call(thread_id)).await;
    targeted_request(&mut rx, ConnectionId(1), &old_id).await;
    outgoing.connection_closed(ConnectionId(1)).await;
    closed(old_waiter).await;
    assert!(
        !outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&old_id)
    );

    // A disconnected name remains reserved, even on the broadcast API.
    let (reserved_id, reserved_waiter) = outgoing.send_request(call(thread_id)).await;
    closed(reserved_waiter).await;
    no_message(&mut rx);
    assert!(
        !outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&reserved_id)
    );

    {
        let mut ownership = outgoing.dynamic_tool_ownership.lock().await;
        let update = ownership
            .plan(thread_id, ConnectionId(3), &tools, vec![tool()], tokens)
            .expect("reconnect using the saved capability");
        ownership.commit(thread_id, update);
    }
    outgoing
        .replay_requests_to_connection_for_thread(ConnectionId(3), thread_id)
        .await;
    no_message(&mut rx);
    outgoing
        .notify_client_response(ConnectionId(3), old_id, answer())
        .await;
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());

    let reconnected = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(2), ConnectionId(3)],
        thread_id,
    );
    let (id, mut waiter) = reconnected.send_request(call(thread_id)).await;
    targeted_request(&mut rx, ConnectionId(3), &id).await;
    no_message(&mut rx);
    outgoing
        .notify_client_response(ConnectionId(1), id.clone(), answer())
        .await;
    pending(&mut waiter);
    let error = internal_error("owner failure");
    outgoing
        .notify_client_error(ConnectionId(3), id, error.clone())
        .await;
    assert_eq!(
        timeout(WAIT, waiter)
            .await
            .expect("owner error should resolve")
            .expect("callback should deliver"),
        Err(error)
    );
}

#[tokio::test]
async fn unloaded_catalog_keeps_routing_reserved_until_explicit_token_reclaim() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    let (_, tokens) = register(&outgoing, thread_id).await;
    {
        let mut ownership = outgoing.dynamic_tool_ownership.lock().await;
        ownership.unload(thread_id);
        assert!(ownership.ensure_legacy_set_allowed(thread_id).is_err());
        assert!(
            ownership
                .plan(
                    thread_id,
                    ConnectionId(3),
                    &[],
                    vec![tool()],
                    HashMap::new()
                )
                .is_err()
        );
    }
    let (_, waiter) = outgoing.send_request(call(thread_id)).await;
    closed(waiter).await;
    no_message(&mut rx);
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
    {
        let mut ownership = outgoing.dynamic_tool_ownership.lock().await;
        let update = ownership
            .plan(thread_id, ConnectionId(3), &[], vec![tool()], tokens)
            .expect("explicit reclaim should restore absent runtime tool");
        assert_eq!(update.tools, vec![tool()]);
        ownership.commit(thread_id, update);
    }
    let scoped = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(2), ConnectionId(3)],
        thread_id,
    );
    let (id, waiter) = scoped.send_request(call(thread_id)).await;
    targeted_request(&mut rx, ConnectionId(3), &id).await;
    no_message(&mut rx);
    outgoing
        .notify_client_response(ConnectionId(3), id, answer())
        .await;
    assert_eq!(
        timeout(WAIT, waiter)
            .await
            .expect("reclaimed owner response should resolve")
            .expect("callback should deliver"),
        Ok(answer())
    );
}

#[tokio::test]
async fn unsubscribed_owner_fails_closed_without_callback_or_broadcast() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    register(&outgoing, thread_id).await;
    for subscribers in [vec![ConnectionId(2)], vec![]] {
        let scoped =
            ThreadScopedOutgoingMessageSender::new(outgoing.clone(), subscribers, thread_id);
        let (_, waiter) = scoped.send_request(call(thread_id)).await;
        closed(waiter).await;
        no_message(&mut rx);
        assert!(outgoing.request_id_to_callback.lock().await.is_empty());
    }
}

#[tokio::test]
async fn legacy_call_fans_out_replays_and_accepts_any_connection_response() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    let scoped = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(1), ConnectionId(2)],
        thread_id,
    );
    let (id, waiter) = scoped.send_request(call(thread_id)).await;
    targeted_request(&mut rx, ConnectionId(1), &id).await;
    targeted_request(&mut rx, ConnectionId(2), &id).await;
    no_message(&mut rx);
    let requests = outgoing.pending_requests_for_thread(thread_id).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].id(), &id);
    outgoing.connection_closed(ConnectionId(1)).await;
    outgoing
        .replay_requests_to_connection_for_thread(ConnectionId(3), thread_id)
        .await;
    targeted_request(&mut rx, ConnectionId(3), &id).await;
    outgoing
        .notify_client_response(ConnectionId(3), id, answer())
        .await;
    assert_eq!(
        timeout(WAIT, waiter)
            .await
            .expect("legacy response should resolve")
            .expect("callback should deliver"),
        Ok(answer())
    );
    assert!(
        outgoing
            .pending_requests_for_thread(thread_id)
            .await
            .is_empty()
    );

    // The global legacy API retains its broadcast and error-resolution behavior.
    let (id, waiter) = outgoing.send_request(call(thread_id)).await;
    let envelope = timeout(WAIT, rx.recv())
        .await
        .expect("broadcast should arrive")
        .expect("queue open");
    assert!(
        matches!(envelope, OutgoingEnvelope::Broadcast { message: OutgoingMessage::Request(ref request) } if request.id() == &id)
    );
    let error = internal_error("legacy failure");
    outgoing
        .notify_client_error(ConnectionId(2), id, error.clone())
        .await;
    assert_eq!(
        timeout(WAIT, waiter)
            .await
            .expect("legacy error should resolve")
            .expect("callback should deliver"),
        Err(error)
    );
    no_message(&mut rx);
}

#[tokio::test]
async fn dispatch_waits_for_catalog_commit_before_selecting_owner() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    let mut ownership = outgoing.dynamic_tool_ownership.lock().await;
    let update = ownership
        .plan(
            thread_id,
            ConnectionId(1),
            &[],
            vec![tool()],
            HashMap::new(),
        )
        .expect("plan catalog update");
    let scoped = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(1), ConnectionId(2)],
        thread_id,
    );
    let dispatch = scoped.send_request(call(thread_id));
    tokio::pin!(dispatch);
    // Poll exactly once while the catalog lock is held; no timing assumptions.
    tokio::select! {
        biased;
        _ = &mut dispatch => panic!("dispatch bypassed catalog lock"),
        _ = std::future::ready(()) => {}
    }
    no_message(&mut rx);
    ownership.commit(thread_id, update);
    drop(ownership);
    let (id, waiter) = timeout(WAIT, dispatch)
        .await
        .expect("commit should unblock dispatch");
    targeted_request(&mut rx, ConnectionId(1), &id).await;
    no_message(&mut rx);
    outgoing.connection_closed(ConnectionId(1)).await;
    closed(waiter).await;
}

#[tokio::test]
async fn dispatch_registration_cannot_escape_disconnect_cleanup() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    register(&outgoing, thread_id).await;
    let callbacks = outgoing.request_id_to_callback.lock().await;
    let scoped = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(1), ConnectionId(2)],
        thread_id,
    );
    let dispatch = scoped.send_request(call(thread_id));
    tokio::pin!(dispatch);
    tokio::select! {
        biased;
        _ = &mut dispatch => panic!("dispatch bypassed callback lock"),
        _ = std::future::ready(()) => {}
    }
    assert!(outgoing.dynamic_tool_ownership.try_lock().is_err());
    let disconnect = outgoing.connection_closed(ConnectionId(1));
    tokio::pin!(disconnect);
    tokio::select! {
        biased;
        _ = &mut disconnect => panic!("disconnect bypassed dispatch ownership lock"),
        _ = std::future::ready(()) => {}
    }
    drop(callbacks);
    let (id, waiter) = timeout(WAIT, dispatch)
        .await
        .expect("dispatch should finish");
    timeout(WAIT, disconnect)
        .await
        .expect("disconnect should finish");
    targeted_request(&mut rx, ConnectionId(1), &id).await;
    closed(waiter).await;
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
    no_message(&mut rx);
}

#[tokio::test]
async fn disconnect_before_registration_prevents_dispatch_and_closed_owner_commit() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    let (tools, _) = register(&outgoing, thread_id).await;
    let callbacks = outgoing.request_id_to_callback.lock().await;
    let disconnect = outgoing.connection_closed(ConnectionId(1));
    tokio::pin!(disconnect);
    tokio::select! {
        biased;
        _ = &mut disconnect => panic!("disconnect bypassed callback lock"),
        _ = std::future::ready(()) => {}
    }
    assert!(outgoing.dynamic_tool_ownership.try_lock().is_err());
    let scoped = ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(1), ConnectionId(2)],
        thread_id,
    );
    let dispatch = scoped.send_request(call(thread_id));
    tokio::pin!(dispatch);
    tokio::select! {
        biased;
        _ = &mut dispatch => panic!("dispatch bypassed disconnect ownership lock"),
        _ = std::future::ready(()) => {}
    }
    drop(callbacks);
    timeout(WAIT, disconnect)
        .await
        .expect("disconnect should finish");
    let (_, waiter) = timeout(WAIT, dispatch)
        .await
        .expect("dispatch should fail promptly");
    closed(waiter).await;
    no_message(&mut rx);
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
    let ownership = outgoing.dynamic_tool_ownership.lock().await;
    assert!(
        ownership
            .plan(
                thread_id,
                ConnectionId(1),
                &tools,
                vec![tool()],
                HashMap::new()
            )
            .is_err()
    );
}

#[tokio::test]
async fn verification_revocation_does_not_wait_for_dynamic_catalog_lock() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    register(&outgoing, thread_id).await;
    outgoing
        .enable_user_verification_connection(ConnectionId(1))
        .await;
    let ownership = outgoing.dynamic_tool_ownership.lock().await;
    let disconnect = outgoing.disconnect_connection_owned_requests(ConnectionId(1));
    tokio::pin!(disconnect);
    tokio::select! {
        biased;
        _ = &mut disconnect => panic!("disconnect bypassed catalog lock"),
        _ = std::future::ready(()) => {}
    }
    assert!(
        outgoing
            .verification_connections
            .try_lock()
            .expect("eligibility lock should be released")
            .is_empty()
    );
    assert!(outgoing.request_id_to_callback.try_lock().is_ok());
    drop(ownership);
    timeout(WAIT, disconnect)
        .await
        .expect("disconnect should finish after catalog unlock");
    let ownership = outgoing.dynamic_tool_ownership.lock().await;
    assert!(matches!(
        ownership.route(thread_id, None, "owned_tool"),
        crate::dynamic_tool_ownership::ToolRoute::Owned(None)
    ));
    no_message(&mut rx);
}

#[tokio::test]
async fn disconnect_revokes_both_owner_fences_before_waiting_for_callbacks() {
    for early_disconnect in [false, true] {
        let (outgoing, mut rx) = setup();
        let thread_id = ThreadId::new();
        let (tools, _) = register(&outgoing, thread_id).await;
        outgoing
            .enable_user_verification_connection(ConnectionId(1))
            .await;
        let scoped = ThreadScopedOutgoingMessageSender::new(
            outgoing.clone(),
            vec![ConnectionId(1)],
            thread_id,
        );
        let (owned_id, owned_waiter) = scoped.send_request(call(thread_id)).await;
        targeted_request(&mut rx, ConnectionId(1), &owned_id).await;
        let verification_payload = ServerRequestPayload::McpServerElicitationRequest(
            codex_app_server_protocol::McpServerElicitationRequestParams {
                thread_id: thread_id.to_string(),
                turn_id: Some("turn-1".into()),
                server_name: "test-service".into(),
                request: codex_app_server_protocol::McpServerElicitationRequest::UserVerification {
                    meta: None,
                    title: "Approve".into(),
                    description: String::new(),
                    challenge: "AQID".into(),
                },
            },
        );
        let (ceremony_id, ceremony_waiter) =
            scoped.send_request(verification_payload.clone()).await;
        let envelope = timeout(WAIT, rx.recv())
            .await
            .expect("ceremony should arrive")
            .expect("queue open");
        assert!(matches!(envelope, OutgoingEnvelope::ToConnection {
            connection_id: ConnectionId(1),
            message: OutgoingMessage::Request(ref request),
            ..
        } if request.id() == &ceremony_id));
        let verification = scoped.send_request(verification_payload);
        tokio::pin!(verification);
        let callbacks = outgoing.request_id_to_callback.lock().await;
        tokio::select! {
            biased;
            _ = &mut verification => panic!("registration bypassed callback lock"),
            _ = std::future::ready(()) => {}
        }
        let disconnect = async {
            if early_disconnect {
                // Match MessageProcessor's first outgoing disconnect call.
                outgoing
                    .disconnect_connection_owned_requests(ConnectionId(1))
                    .await;
            } else {
                outgoing.connection_closed(ConnectionId(1)).await;
            }
        };
        tokio::pin!(disconnect);
        tokio::select! {
            biased;
            _ = &mut disconnect => panic!("disconnect bypassed callback lock"),
            _ = std::future::ready(()) => {}
        }
        assert!(
            outgoing
                .verification_connections
                .try_lock()
                .expect("verification eligibility lock should be released")
                .is_empty()
        );
        assert!(outgoing.dynamic_tool_ownership.try_lock().is_err());
        assert!(callbacks.contains_key(&owned_id));
        assert!(callbacks.contains_key(&ceremony_id));
        no_message(&mut rx);
        drop(callbacks);
        let ((verification_id, verification_waiter), ()) =
            timeout(WAIT, async { tokio::join!(verification, disconnect) })
                .await
                .expect("registration and disconnect should finish");
        closed(owned_waiter).await;
        closed(ceremony_waiter).await;
        closed(verification_waiter).await;
        no_message(&mut rx);
        let callbacks = outgoing.request_id_to_callback.lock().await;
        assert!(!callbacks.contains_key(&owned_id));
        assert!(!callbacks.contains_key(&ceremony_id));
        assert!(!callbacks.contains_key(&verification_id));
        drop(callbacks);
        let ownership = outgoing.dynamic_tool_ownership.lock().await;
        assert!(matches!(
            ownership.route(thread_id, None, "owned_tool"),
            crate::dynamic_tool_ownership::ToolRoute::Owned(None)
        ));
        assert!(
            ownership
                .plan(
                    thread_id,
                    ConnectionId(1),
                    &tools,
                    vec![tool()],
                    HashMap::new()
                )
                .is_err()
        );
    }
}

#[tokio::test]
async fn early_owned_unload_preserves_legacy_until_full_thread_cancellation() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    register(&outgoing, thread_id).await;
    let scoped =
        ThreadScopedOutgoingMessageSender::new(outgoing.clone(), vec![ConnectionId(1)], thread_id);
    let (owned_id, owned_waiter) = scoped.send_request(call(thread_id)).await;
    targeted_request(&mut rx, ConnectionId(1), &owned_id).await;
    let mut legacy_call = call(thread_id);
    let ServerRequestPayload::DynamicToolCall(params) = &mut legacy_call else {
        panic!("expected dynamic tool call");
    };
    params.tool = "legacy_tool".into();
    let (legacy_id, mut legacy_waiter) = scoped.send_request(legacy_call).await;
    targeted_request(&mut rx, ConnectionId(1), &legacy_id).await;

    outgoing.unload_dynamic_tool_thread(thread_id).await;
    closed(owned_waiter).await;
    pending(&mut legacy_waiter);
    let callbacks = outgoing.request_id_to_callback.lock().await;
    assert!(!callbacks.contains_key(&owned_id));
    assert!(callbacks.contains_key(&legacy_id));
    drop(callbacks);
    let requests = outgoing.pending_requests_for_thread(thread_id).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].id(), &legacy_id);
    let (_, rejected_waiter) = scoped.send_request(call(thread_id)).await;
    closed(rejected_waiter).await;
    no_message(&mut rx);

    outgoing.cancel_requests_for_thread(thread_id, None).await;
    closed(legacy_waiter).await;
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
}

#[tokio::test]
async fn revoked_owner_cannot_answer_before_disconnect_callback_cleanup() {
    for send_error in [false, true] {
        let (outgoing, mut rx) = setup();
        let thread_id = ThreadId::new();
        register(&outgoing, thread_id).await;
        let scoped = ThreadScopedOutgoingMessageSender::new(
            outgoing.clone(),
            vec![ConnectionId(1), ConnectionId(2)],
            thread_id,
        );
        let (id, mut waiter) = scoped.send_request(call(thread_id)).await;
        targeted_request(&mut rx, ConnectionId(1), &id).await;
        // Exercise the interval between revocation and callback cleanup.
        outgoing
            .dynamic_tool_ownership
            .lock()
            .await
            .disconnect(ConnectionId(1));
        outgoing
            .notify_client_response(ConnectionId(2), id.clone(), answer())
            .await;
        pending(&mut waiter);
        assert!(
            outgoing
                .request_id_to_callback
                .lock()
                .await
                .contains_key(&id)
        );
        if send_error {
            outgoing
                .notify_client_error(
                    ConnectionId(1),
                    id.clone(),
                    internal_error("late owner error"),
                )
                .await;
        } else {
            outgoing
                .notify_client_response(ConnectionId(1), id.clone(), answer())
                .await;
        }
        closed(waiter).await;
        assert!(
            !outgoing
                .request_id_to_callback
                .lock()
                .await
                .contains_key(&id)
        );
        no_message(&mut rx);
    }
}

#[tokio::test]
async fn unloaded_owner_cannot_answer_before_thread_callback_cleanup() {
    for send_error in [false, true] {
        let (outgoing, mut rx) = setup();
        let thread_id = ThreadId::new();
        register(&outgoing, thread_id).await;
        let scoped = ThreadScopedOutgoingMessageSender::new(
            outgoing.clone(),
            vec![ConnectionId(1)],
            thread_id,
        );
        let (id, waiter) = scoped.send_request(call(thread_id)).await;
        targeted_request(&mut rx, ConnectionId(1), &id).await;
        outgoing
            .dynamic_tool_ownership
            .lock()
            .await
            .unload(thread_id);
        if send_error {
            outgoing
                .notify_client_error(
                    ConnectionId(1),
                    id.clone(),
                    internal_error("late unloaded error"),
                )
                .await;
        } else {
            outgoing
                .notify_client_response(ConnectionId(1), id.clone(), answer())
                .await;
        }
        closed(waiter).await;
        assert!(
            !outgoing
                .request_id_to_callback
                .lock()
                .await
                .contains_key(&id)
        );
        no_message(&mut rx);
    }
}

#[tokio::test]
async fn owned_answer_waits_for_revocation_fence_before_consuming_callback() {
    let (outgoing, mut rx) = setup();
    let thread_id = ThreadId::new();
    register(&outgoing, thread_id).await;
    let scoped =
        ThreadScopedOutgoingMessageSender::new(outgoing.clone(), vec![ConnectionId(1)], thread_id);
    let (id, mut waiter) = scoped.send_request(call(thread_id)).await;
    targeted_request(&mut rx, ConnectionId(1), &id).await;
    let mut ownership = outgoing.dynamic_tool_ownership.lock().await;
    let response = outgoing.notify_client_response(ConnectionId(1), id.clone(), answer());
    tokio::pin!(response);
    tokio::select! {
        biased;
        _ = &mut response => panic!("owned answer bypassed ownership fence"),
        _ = std::future::ready(()) => {}
    }
    // The blocked answer must release callbacks rather than invert lock order.
    assert!(
        outgoing
            .request_id_to_callback
            .try_lock()
            .expect("callback lock should be available")
            .contains_key(&id)
    );
    pending(&mut waiter);
    ownership.unload(thread_id);
    drop(ownership);
    timeout(WAIT, response)
        .await
        .expect("revocation should unblock answer");
    closed(waiter).await;
    assert!(
        !outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&id)
    );
    no_message(&mut rx);
}

#[tokio::test]
async fn owned_queue_backpressure_fails_without_blocking_disconnect_or_replay() {
    let (tx, mut rx) = mpsc::channel(1);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        AnalyticsEventsClient::disabled(),
    ));
    let thread = ThreadId::new();
    register(&outgoing, thread).await;
    let scoped =
        ThreadScopedOutgoingMessageSender::new(outgoing.clone(), vec![ConnectionId(1)], thread);
    let (first_id, first) = scoped.send_request(call(thread)).await;
    let (second_id, second) = scoped.send_request(call(thread)).await;
    closed(second).await;
    assert!(
        !outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&second_id)
    );
    outgoing.connection_closed(ConnectionId(1)).await;
    closed(first).await;
    targeted_request(&mut rx, ConnectionId(1), &first_id).await;
    outgoing
        .replay_requests_to_connection_for_thread(ConnectionId(2), thread)
        .await;
    no_message(&mut rx);
}
