//! Runtime-only connection ownership. Capabilities never enter core tool metadata.
use std::collections::HashMap;
use std::collections::HashSet;

use codex_app_server_protocol::DynamicToolSpec;
use codex_protocol::ThreadId;
use uuid::Uuid;

use crate::outgoing_message::ConnectionId;

#[derive(Clone)]
struct Registration {
    owner: Option<ConnectionId>,
    // Deliberately no Debug implementation: this is a reconnect capability.
    token: String,
}

#[derive(Clone, Default)]
struct ThreadOwnership {
    registrations: HashMap<String, Registration>,
}

#[derive(Default)]
pub(crate) struct DynamicToolOwnership {
    threads: HashMap<ThreadId, ThreadOwnership>,
    closed: HashSet<ConnectionId>,
}

pub(crate) enum ToolRoute {
    Legacy,
    Owned(Option<ConnectionId>),
}

pub(crate) struct OwnershipUpdate {
    pub(crate) tools: Vec<DynamicToolSpec>,
    pub(crate) tokens: HashMap<String, String>,
    next: ThreadOwnership,
}

pub(crate) fn tool_name(tool: &DynamicToolSpec) -> &str {
    match tool {
        DynamicToolSpec::Function(tool) => &tool.name,
        DynamicToolSpec::Namespace(namespace) => &namespace.name,
    }
}

impl DynamicToolOwnership {
    pub(crate) fn plan(
        &self,
        thread_id: ThreadId,
        connection: ConnectionId,
        current: &[DynamicToolSpec],
        requested: Vec<DynamicToolSpec>,
        reconnect: HashMap<String, String>,
    ) -> Result<OwnershipUpdate, String> {
        if self.closed.contains(&connection) {
            return Err("dynamic tool owner connection is closed".into());
        }
        let mut next = self.threads.get(&thread_id).cloned().unwrap_or_default();
        let names: HashSet<_> = requested.iter().map(tool_name).map(str::to_owned).collect();
        if names.len() != requested.len() {
            return Err("duplicate owned dynamic tool name".into());
        }
        if reconnect.keys().any(|name| !names.contains(name)) {
            return Err("reconnect capability must name a requested tool".into());
        }
        let current_names: HashSet<_> = current.iter().map(tool_name).collect();
        if next.registrations.iter().any(|(name, registration)| {
            registration.owner.is_some() && !current_names.contains(name.as_str())
        }) {
            return Err("owned dynamic tool catalog no longer matches runtime".into());
        }
        for name in &names {
            match next.registrations.get(name) {
                Some(registration) if registration.owner == Some(connection) => {
                    if reconnect.get(name).is_some_and(|token| {
                        !constant_time_eq::constant_time_eq(
                            token.as_bytes(),
                            registration.token.as_bytes(),
                        )
                    }) {
                        return Err("invalid dynamic tool reconnect capability".into());
                    }
                }
                Some(registration) if registration.owner.is_none() => {
                    if !reconnect.get(name).is_some_and(|token| {
                        constant_time_eq::constant_time_eq(
                            token.as_bytes(),
                            registration.token.as_bytes(),
                        )
                    }) {
                        return Err(
                            "dynamic tool name is reserved; reconnect capability required".into(),
                        );
                    }
                }
                Some(_) => return Err("dynamic tool name belongs to another connection".into()),
                None if current_names.contains(name.as_str()) => {
                    return Err("legacy dynamic tool cannot acquire ownership".into());
                }
                None if reconnect.contains_key(name) => {
                    return Err("retired or unknown dynamic tool reconnect capability".into());
                }
                None => {}
            }
        }
        // Clear only this live connection's set. Disconnected reservations and
        // other owners remain, including when requested is empty.
        let removed: HashSet<_> = next
            .registrations
            .iter()
            .filter(|(_, registration)| registration.owner == Some(connection))
            .map(|(name, _)| name.clone())
            .collect();
        let mut tools: Vec<_> = current
            .iter()
            .filter(|tool| !removed.contains(tool_name(tool)) && !names.contains(tool_name(tool)))
            .cloned()
            .collect();
        let mut tokens = HashMap::new();
        for name in &names {
            let registration =
                next.registrations
                    .entry(name.clone())
                    .or_insert_with(|| Registration {
                        owner: None,
                        // Two independently random UUIDv4 values provide 244 random bits.
                        token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
                    });
            registration.owner = Some(connection);
            tokens.insert(name.clone(), registration.token.clone());
        }
        next.registrations
            .retain(|name, _| !removed.contains(name) || names.contains(name));
        tools.extend(requested);
        Ok(OwnershipUpdate {
            tools,
            tokens,
            next,
        })
    }

    pub(crate) fn commit(&mut self, thread_id: ThreadId, update: OwnershipUpdate) {
        if update.next.registrations.is_empty() {
            self.threads.remove(&thread_id);
        } else {
            self.threads.insert(thread_id, update.next);
        }
    }

    pub(crate) fn ensure_legacy_set_allowed(&self, thread_id: ThreadId) -> Result<(), String> {
        if self
            .threads
            .get(&thread_id)
            .is_some_and(|thread| !thread.registrations.is_empty())
        {
            Err("legacy setter cannot replace a catalog containing owned dynamic tools".into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn route(
        &self,
        thread_id: ThreadId,
        namespace: Option<&str>,
        tool: &str,
    ) -> ToolRoute {
        match self
            .threads
            .get(&thread_id)
            .and_then(|thread| thread.registrations.get(namespace.unwrap_or(tool)))
        {
            Some(registration) => ToolRoute::Owned(registration.owner),
            None => ToolRoute::Legacy,
        }
    }

    pub(crate) fn disconnect(&mut self, connection: ConnectionId) {
        self.closed.insert(connection);
        for thread in self.threads.values_mut() {
            for registration in thread.registrations.values_mut() {
                if registration.owner == Some(connection) {
                    registration.owner = None;
                }
            }
        }
    }

    /// Unload removes live ownership, but retains name reservations and capabilities.
    /// Resume must explicitly reclaim runtime tools; late calls cannot fall through.
    pub(crate) fn unload(&mut self, thread_id: ThreadId) {
        if let Some(thread) = self.threads.get_mut(&thread_id) {
            for registration in thread.registrations.values_mut() {
                registration.owner = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::DynamicToolFunctionSpec;
    use serde_json::json;

    fn tool(name: &str) -> DynamicToolSpec {
        DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: name.into(),
            description: "test".into(),
            input_schema: json!({"type":"object"}),
            defer_loading: false,
        })
    }

    fn set(
        state: &mut DynamicToolOwnership,
        thread: ThreadId,
        owner: ConnectionId,
        current: &[DynamicToolSpec],
        tools: Vec<DynamicToolSpec>,
        tokens: HashMap<String, String>,
    ) -> (Vec<DynamicToolSpec>, HashMap<String, String>) {
        let update = state.plan(thread, owner, current, tools, tokens).unwrap();
        let catalog = update.tools.clone();
        let capabilities = update.tokens.clone();
        state.commit(thread, update);
        (catalog, capabilities)
    }

    #[test]
    fn scoped_replace_clear_preserves_legacy_and_other_owner_and_retires_tokens() {
        let mut state = DynamicToolOwnership::default();
        let thread = ThreadId::new();
        let (catalog, old) = set(
            &mut state,
            thread,
            ConnectionId(1),
            &[tool("legacy")],
            vec![tool("first")],
            HashMap::new(),
        );
        let (catalog, _) = set(
            &mut state,
            thread,
            ConnectionId(2),
            &catalog,
            vec![tool("second")],
            HashMap::new(),
        );
        let (catalog, _) = set(
            &mut state,
            thread,
            ConnectionId(1),
            &catalog,
            vec![],
            HashMap::new(),
        );
        assert_eq!(
            catalog.iter().map(tool_name).collect::<Vec<_>>(),
            vec!["legacy", "second"]
        );
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(3),
                    &catalog,
                    vec![tool("first")],
                    old.clone()
                )
                .is_err()
        );
        let (catalog, fresh) = set(
            &mut state,
            thread,
            ConnectionId(3),
            &catalog,
            vec![tool("first")],
            HashMap::new(),
        );
        assert_ne!(fresh["first"], old["first"]);
        state.disconnect(ConnectionId(3));
        assert!(
            state
                .plan(thread, ConnectionId(4), &catalog, vec![tool("first")], old)
                .is_err()
        );
        assert!(state.ensure_legacy_set_allowed(thread).is_err());
    }

    #[test]
    fn no_legacy_claim_name_collision_or_extraneous_capability() {
        let mut state = DynamicToolOwnership::default();
        let thread = ThreadId::new();
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(1),
                    &[tool("legacy")],
                    vec![tool("legacy")],
                    HashMap::new()
                )
                .is_err()
        );
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(1),
                    &[],
                    vec![tool("new")],
                    HashMap::from([("legacy".into(), "secret".into())])
                )
                .is_err()
        );
        let (catalog, tokens) = set(
            &mut state,
            thread,
            ConnectionId(1),
            &[],
            vec![tool("owned")],
            HashMap::new(),
        );
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(2),
                    &catalog,
                    vec![tool("owned")],
                    tokens
                )
                .is_err()
        );
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(2),
                    &catalog,
                    vec![tool("dup"), tool("dup")],
                    HashMap::new()
                )
                .is_err()
        );
    }

    #[test]
    fn disconnected_reservation_correct_reconnect_and_closed_connection_fencing() {
        let mut state = DynamicToolOwnership::default();
        let thread = ThreadId::new();
        let (catalog, tokens) = set(
            &mut state,
            thread,
            ConnectionId(1),
            &[],
            vec![tool("owned")],
            HashMap::new(),
        );
        state.disconnect(ConnectionId(1));
        assert!(matches!(
            state.route(thread, None, "owned"),
            ToolRoute::Owned(None)
        ));
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(1),
                    &catalog,
                    vec![tool("owned")],
                    tokens.clone()
                )
                .is_err()
        );
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(2),
                    &catalog,
                    vec![tool("owned")],
                    HashMap::new()
                )
                .is_err()
        );
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(2),
                    &catalog,
                    vec![tool("owned")],
                    HashMap::from([("owned".into(), "wrong".into())])
                )
                .is_err()
        );
        let (_, recovered) = set(
            &mut state,
            thread,
            ConnectionId(2),
            &catalog,
            vec![tool("owned")],
            tokens.clone(),
        );
        assert_eq!(recovered, tokens);
        assert!(matches!(
            state.route(thread, None, "owned"),
            ToolRoute::Owned(Some(ConnectionId(2)))
        ));
    }

    #[test]
    fn disconnected_cleanup_requires_reclaim_then_clear_and_preserves_mixed_catalog() {
        let mut state = DynamicToolOwnership::default();
        let thread = ThreadId::new();
        let (catalog, saved) = set(
            &mut state,
            thread,
            ConnectionId(1),
            &[tool("legacy")],
            vec![tool("broker_operation")],
            HashMap::new(),
        );
        let (catalog, _) = set(
            &mut state,
            thread,
            ConnectionId(2),
            &catalog,
            vec![tool("other_owner")],
            HashMap::new(),
        );
        state.disconnect(ConnectionId(1));

        // A token identifies a requested reclaim, not a deletion selector.
        let error = state
            .plan(thread, ConnectionId(3), &catalog, vec![], saved.clone())
            .err()
            .expect("token-bearing empty set must reject without a cleanup receipt");
        assert_eq!(error, "reconnect capability must name a requested tool");
        assert!(matches!(
            state.route(thread, None, "broker_operation"),
            ToolRoute::Owned(None)
        ));
        assert!(state.ensure_legacy_set_allowed(thread).is_err());

        // Tokenless [] has only live-caller scope. It cannot delete reservations.
        let (unchanged, empty) = set(
            &mut state,
            thread,
            ConnectionId(3),
            &catalog,
            vec![],
            HashMap::new(),
        );
        assert_eq!(unchanged, catalog);
        assert!(empty.is_empty());
        assert!(matches!(
            state.route(thread, None, "broker_operation"),
            ToolRoute::Owned(None)
        ));
        assert!(state.ensure_legacy_set_allowed(thread).is_err());

        let (reclaimed, recovered) = set(
            &mut state,
            thread,
            ConnectionId(3),
            &unchanged,
            vec![tool("broker_operation")],
            saved.clone(),
        );
        assert_eq!(recovered, saved);
        let (cleared, empty) = set(
            &mut state,
            thread,
            ConnectionId(3),
            &reclaimed,
            vec![],
            HashMap::new(),
        );
        assert!(empty.is_empty());
        assert_eq!(cleared, vec![tool("legacy"), tool("other_owner")]);
        assert!(matches!(
            state.route(thread, None, "other_owner"),
            ToolRoute::Owned(Some(ConnectionId(2)))
        ));
        assert!(state.ensure_legacy_set_allowed(thread).is_err());
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(4),
                    &cleared,
                    vec![tool("broker_operation")],
                    saved,
                )
                .is_err()
        );
        let (legacy, _) = set(
            &mut state,
            thread,
            ConnectionId(2),
            &cleared,
            vec![],
            HashMap::new(),
        );
        assert_eq!(legacy, vec![tool("legacy")]);
        assert!(state.ensure_legacy_set_allowed(thread).is_ok());
    }

    #[test]
    fn unload_reserves_names_until_explicit_reclaim_without_fork_inheritance() {
        let mut state = DynamicToolOwnership::default();
        let thread = ThreadId::new();
        let (_, tokens) = set(
            &mut state,
            thread,
            ConnectionId(1),
            &[],
            vec![tool("owned")],
            HashMap::new(),
        );
        state.unload(thread);
        assert!(matches!(
            state.route(thread, None, "owned"),
            ToolRoute::Owned(None)
        ));
        assert!(state.ensure_legacy_set_allowed(thread).is_err());
        assert!(
            state
                .plan(
                    thread,
                    ConnectionId(2),
                    &[],
                    vec![tool("owned")],
                    HashMap::new()
                )
                .is_err()
        );
        let (catalog, _) = set(
            &mut state,
            thread,
            ConnectionId(2),
            &[],
            vec![tool("owned")],
            tokens,
        );
        assert_eq!(catalog, vec![tool("owned")]);
        assert!(matches!(
            state.route(ThreadId::new(), None, "owned"),
            ToolRoute::Legacy
        ));
        let (catalog, _) = set(
            &mut state,
            thread,
            ConnectionId(2),
            &catalog,
            vec![],
            HashMap::new(),
        );
        assert!(catalog.is_empty());
        assert!(state.ensure_legacy_set_allowed(thread).is_ok());
    }
}
