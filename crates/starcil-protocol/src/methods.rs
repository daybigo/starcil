//! The complete raw method catalog (dot notation): the whole socket API
//! surface in one place. The server registers a handler for every name in
//! `ALL`; `api schema` is generated from this registry, so a missing handler
//! is a test failure, not a silent gap.

/// Every public method name. Order groups match the docs.
pub const ALL: &[&str] = &[
    // server
    "ping",
    "server.stop",
    "server.reload_config",
    "server.agent_manifests",
    "server.reload_agent_manifests",
    // notification
    "notification.show",
    // client
    "client.window_title.set",
    "client.window_title.clear",
    // session
    "session.snapshot",
    // workspace
    "workspace.create",
    "workspace.list",
    "workspace.get",
    "workspace.focus",
    "workspace.rename",
    "workspace.move",
    "workspace.move_block",
    "workspace.report_metadata",
    "workspace.close",
    // worktree
    "worktree.list",
    "worktree.create",
    "worktree.open",
    "worktree.remove",
    // tab
    "tab.create",
    "tab.list",
    "tab.get",
    "tab.focus",
    "tab.rename",
    "tab.move",
    "tab.close",
    // pane
    "pane.split",
    "pane.swap",
    "pane.move",
    "pane.zoom",
    "pane.layout",
    "pane.process_info",
    "pane.neighbor",
    "pane.edges",
    "pane.focus_direction",
    "pane.focus",
    "pane.resize",
    "pane.list",
    "pane.current",
    "pane.get",
    "pane.rename",
    "pane.send_text",
    "pane.send_keys",
    "pane.send_input",
    "pane.read",
    "pane.run",
    "pane.graphics.info",
    "pane.graphics.set",
    "pane.graphics.clear",
    "pane.graphics.stream",
    "pane.report_agent",
    "pane.report_agent_session",
    "pane.report_metadata",
    "pane.clear_agent_authority",
    "pane.release_agent",
    "pane.close",
    "pane.wait_for_output",
    // popup
    "popup.close",
    // layout
    "layout.export",
    "layout.apply",
    "layout.set_split_ratio",
    // agent
    "agent.list",
    "agent.get",
    "agent.read",
    "agent.explain",
    "agent.send_keys",
    "agent.prompt",
    "agent.wait",
    "agent.rename",
    "agent.focus",
    "agent.start",
    "agent.attach",
    "agent.view.set",
    "agent.view.clear",
    // events
    "events.subscribe",
    "events.wait",
    // integration
    "integration.install",
    "integration.uninstall",
    "integration.status",
    // plugin
    "plugin.link",
    "plugin.list",
    "plugin.unlink",
    "plugin.enable",
    "plugin.disable",
    "plugin.action.list",
    "plugin.action.invoke",
    "plugin.log.list",
    "plugin.pane.open",
    "plugin.pane.focus",
    "plugin.pane.close",
];

pub fn is_known(method: &str) -> bool {
    ALL.contains(&method)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_no_duplicates() {
        let mut seen = std::collections::BTreeSet::new();
        for m in ALL {
            assert!(seen.insert(*m), "duplicate method {m}");
        }
    }

    #[test]
    fn core_methods_present() {
        for m in [
            "ping",
            "pane.split",
            "agent.prompt",
            "session.snapshot",
            "layout.apply",
            "events.subscribe",
            "worktree.create",
            "plugin.link",
        ] {
            assert!(is_known(m), "missing {m}");
        }
        assert!(!is_known("pane.explode"));
    }
}
