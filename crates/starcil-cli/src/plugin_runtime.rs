use crate::{endpoint_for, Connection, EndpointSelection, GithubSlug, PluginAction, PluginTarget};
use serde_json::{json, Value};
use starcil_plugins::{
    ActiveContext, HostEnvironment, LogStore, ManifestValidator, PluginEntry, PluginExecutor,
    PluginRegistry, RegistryPaths, SourceMetadata,
};
use starcil_protocol::{Incoming, Request};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn dispatch_plugin(
    action: PluginAction,
    session: Option<String>,
    mut connection: Option<Box<dyn Connection>>,
) -> i32 {
    let session_name = session.clone().unwrap_or_else(|| "default".to_owned());
    let paths = match starcil_platform::PlatformPaths::discover() {
        Ok(paths) => paths,
        Err(error) => return plugin_error(error),
    };
    if let Err(error) = paths.session_runtime_dir(&session_name) {
        return plugin_error(error);
    }
    let selection = EndpointSelection { session };

    let result = match action {
        PluginAction::Install { source, requested_ref, yes } => {
            install_plugin(&paths, &session_name, &mut connection, source, requested_ref, yes)
        }
        PluginAction::Uninstall { target } => {
            uninstall_plugin(&paths, &session_name, &mut connection, &target)
        }
        PluginAction::Link { path, disabled } => {
            link_plugin(&paths, &session_name, &mut connection, &path, !disabled, None)
                .map(|entry| print_plugin_result("linked", &entry))
        }
        PluginAction::List { plugin_id, json } => {
            list_plugins(&paths, &session_name, &mut connection)
                .map(|plugins| render_plugin_list(plugins, plugin_id.as_deref(), json))
        }
        PluginAction::ConfigDir { plugin_id } => {
            list_plugins(&paths, &session_name, &mut connection).and_then(|plugins| {
                let entry = plugins
                    .into_iter()
                    .find(|entry| entry.plugin_id == plugin_id)
                    .ok_or_else(|| format!("plugin '{plugin_id}' was not found"))?;
                println!("{}", entry.config_dir.display());
                Ok(())
            })
        }
        PluginAction::Unlink { plugin_id } => mutate_registration(
            &paths,
            &session_name,
            &mut connection,
            "plugin.unlink",
            &plugin_id,
        )
        .map(|entry| print_plugin_result("unlinked", &entry)),
        PluginAction::Enable { plugin_id } => mutate_registration(
            &paths,
            &session_name,
            &mut connection,
            "plugin.enable",
            &plugin_id,
        )
        .map(|entry| print_plugin_result("enabled", &entry)),
        PluginAction::Disable { plugin_id } => mutate_registration(
            &paths,
            &session_name,
            &mut connection,
            "plugin.disable",
            &plugin_id,
        )
        .map(|entry| print_plugin_result("disabled", &entry)),
        PluginAction::ActionList { plugin_id } => {
            list_actions(&paths, &session_name, &selection, &mut connection, plugin_id.as_deref())
                .map(render_action_list)
        }
        PluginAction::ActionInvoke { action_id, plugin_id } => {
            let action_id = qualify_action_id(&action_id, plugin_id.as_deref());
            invoke_action(&paths, &session_name, &selection, &mut connection, &action_id, plugin_id.as_deref())
                .map(|result| println!("{}", result))
        }
        PluginAction::LogList { plugin_id, limit } => {
            list_logs(&paths, &session_name, &selection, &mut connection, plugin_id.as_deref(), limit)
                .map(render_log_list)
        }
        PluginAction::PaneOpen { params } => socket_only(&mut connection, "plugin.pane.open", params)
            .map(|result| println!("{}", result)),
        PluginAction::PaneFocus { pane_id } => socket_only(
            &mut connection,
            "plugin.pane.focus",
            json!({"pane_id": pane_id}),
        )
        .map(|result| println!("{}", result)),
        PluginAction::PaneClose { pane_id } => socket_only(
            &mut connection,
            "plugin.pane.close",
            json!({"pane_id": pane_id}),
        )
        .map(|result| println!("{}", result)),
    };

    match result {
        Ok(()) => 0,
        Err(error) => plugin_error(error),
    }
}

fn install_plugin(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    connection: &mut Option<Box<dyn Connection>>,
    source: GithubSlug,
    requested_ref: Option<String>,
    yes: bool,
) -> Result<(), String> {
    let managed_root = managed_github_root(paths);
    fs::create_dir_all(managed_root.join(&source.owner)).map_err(|error| error.to_string())?;
    let checkout = managed_root.join(&source.owner).join(&source.repo);
    if checkout.exists() {
        return Err(format!(
            "managed checkout already exists at {}; uninstall it before reinstalling",
            checkout.display()
        ));
    }

    let repository = format!("https://github.com/{}/{}.git", source.owner, source.repo);
    let checkout_text = checkout.to_string_lossy().into_owned();
    if let Err(error) = run_git(["clone", "--depth", "1", &repository, &checkout_text]) {
        let _ = cleanup_new_checkout(&managed_root, &checkout);
        return Err(error);
    }
    if let Some(reference) = requested_ref.as_deref() {
        let checkout_text = checkout.to_string_lossy().into_owned();
        if let Err(error) = run_git(["-C", &checkout_text, "fetch", "--depth", "1", "origin", reference])
            .and_then(|_| run_git(["-C", &checkout_text, "checkout", "--detach", "FETCH_HEAD"]))
        {
            let _ = cleanup_new_checkout(&managed_root, &checkout);
            return Err(error);
        }
    }

    let resolved_commit = match git_stdout(["-C", &checkout_text, "rev-parse", "HEAD"]) {
        Ok(commit) => commit,
        Err(error) => {
            let _ = cleanup_new_checkout(&managed_root, &checkout);
            return Err(error);
        }
    };
    let plugin_path = source
        .subdir
        .as_deref()
        .map(|subdir| checkout.join(subdir))
        .unwrap_or_else(|| checkout.clone());
    let plugin_path = match canonical_child(&checkout, &plugin_path) {
        Ok(path) => path,
        Err(error) => {
            let _ = cleanup_new_checkout(&managed_root, &checkout);
            return Err(error);
        }
    };
    let loaded = match starcil_plugins::load_manifest(&plugin_path, &ManifestValidator::for_current_binary()) {
        Ok(loaded) => loaded,
        Err(error) => {
            let _ = cleanup_new_checkout(&managed_root, &checkout);
            return Err(error.to_string());
        }
    };

    println!("Plugin: {} ({})", loaded.manifest.name, loaded.manifest.id);
    println!("Source: https://github.com/{}", source.as_str());
    if !yes {
        print!("Link and enable this plugin? [y/N]");
        io::stdout().flush().map_err(|error| error.to_string())?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(|error| error.to_string())?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            cleanup_new_checkout(&managed_root, &checkout)?;
            println!("Plugin installation cancelled.");
            return Ok(());
        }
    }

    let source_metadata = SourceMetadata::Github {
        owner: source.owner,
        repo: source.repo,
        subdir: source.subdir,
        requested_ref,
        resolved_commit,
        managed_path: checkout.to_string_lossy().into_owned(),
        installed_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    };
    let entry = link_plugin(
        paths,
        session,
        connection,
        &plugin_path.to_string_lossy(),
        true,
        Some(source_metadata),
    )?;
    print_plugin_result("installed", &entry);
    Ok(())
}

fn uninstall_plugin(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    connection: &mut Option<Box<dyn Connection>>,
    target: &PluginTarget,
) -> Result<(), String> {
    let entry = list_plugins(paths, session, connection)?
        .into_iter()
        .find(|entry| target_matches(target, entry))
        .ok_or_else(|| format!("plugin '{}' was not found", target_label(target)))?;
    let removed = mutate_registration(
        paths,
        session,
        connection,
        "plugin.unlink",
        &entry.plugin_id,
    )?;
    if let Some(SourceMetadata::Github { managed_path, .. }) = &removed.source {
        remove_managed_checkout(&managed_github_root(paths), Path::new(managed_path))?;
    }
    print_plugin_result("uninstalled", &removed);
    Ok(())
}

fn link_plugin(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    connection: &mut Option<Box<dyn Connection>>,
    path: &str,
    enabled: bool,
    source: Option<SourceMetadata>,
) -> Result<PluginEntry, String> {
    let path = absolute_existing_path(path)?;
    if let Some(connection) = connection.as_mut() {
        let result = socket_call(
            connection.as_mut(),
            "plugin.link",
            json!({"path": path, "enabled": enabled, "source": source}),
        )?;
        plugin_from_result(&result)
    } else {
        let mut registry = open_registry(paths, session)?;
        registry
            .link(&path, enabled, source)
            .map_err(|error| error.to_string())
    }
}

fn list_plugins(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    connection: &mut Option<Box<dyn Connection>>,
) -> Result<Vec<PluginEntry>, String> {
    if let Some(connection) = connection.as_mut() {
        let result = socket_call(connection.as_mut(), "plugin.list", json!({}))?;
        serde_json::from_value(result.get("plugins").cloned().unwrap_or_else(|| json!([])))
            .map_err(|error| format!("invalid plugin.list response: {error}"))
    } else {
        Ok(open_registry(paths, session)?.entries().to_vec())
    }
}

fn mutate_registration(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    connection: &mut Option<Box<dyn Connection>>,
    method: &str,
    plugin_id: &str,
) -> Result<PluginEntry, String> {
    if let Some(connection) = connection.as_mut() {
        let result = socket_call(connection.as_mut(), method, json!({"plugin_id": plugin_id}))?;
        plugin_from_result(&result)
    } else {
        let mut registry = open_registry(paths, session)?;
        match method {
            "plugin.unlink" => registry.unlink(plugin_id),
            "plugin.enable" => registry.enable(plugin_id),
            "plugin.disable" => registry.disable(plugin_id),
            _ => unreachable!("known registration mutation"),
        }
        .map_err(|error| error.to_string())
    }
}

fn list_actions(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    selection: &EndpointSelection,
    connection: &mut Option<Box<dyn Connection>>,
    plugin_id: Option<&str>,
) -> Result<Value, String> {
    if let Some(connection) = connection.as_mut() {
        socket_call(connection.as_mut(), "plugin.action.list", json!({"plugin_id": plugin_id}))
    } else {
        let registry = open_registry(paths, session)?;
        let executor = local_executor(selection)?;
        Ok(json!({"type": "plugin_action_list", "actions": executor.action_list(&registry, plugin_id)}))
    }
}

fn invoke_action(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    selection: &EndpointSelection,
    connection: &mut Option<Box<dyn Connection>>,
    action_id: &str,
    plugin_id: Option<&str>,
) -> Result<Value, String> {
    if let Some(connection) = connection.as_mut() {
        socket_call(
            connection.as_mut(),
            "plugin.action.invoke",
            json!({"action_id": action_id, "plugin_id": plugin_id}),
        )
    } else {
        let registry = open_registry(paths, session)?;
        let executor = local_executor(selection)?;
        let invocation = executor
            .invoke_action(&registry, action_id, None, &ActiveContext::default())
            .map_err(|error| error.to_string())?;
        Ok(json!({"type": "plugin_action_invoked", "invocation": invocation}))
    }
}

fn list_logs(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
    selection: &EndpointSelection,
    connection: &mut Option<Box<dyn Connection>>,
    plugin_id: Option<&str>,
    limit: Option<u64>,
) -> Result<Value, String> {
    if let Some(connection) = connection.as_mut() {
        socket_call(
            connection.as_mut(),
            "plugin.log.list",
            json!({"plugin_id": plugin_id, "limit": limit}),
        )
    } else {
        let _registry = open_registry(paths, session)?;
        let executor = local_executor(selection)?;
        let logs = executor
            .logs()
            .list(plugin_id, limit.map(|value| value as usize))
            .map_err(|error| error.to_string())?;
        Ok(json!({"type": "plugin_log_list", "logs": logs}))
    }
}

fn socket_only(
    connection: &mut Option<Box<dyn Connection>>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let connection = connection
        .as_mut()
        .ok_or_else(|| format!("{method} requires a running Starcil server"))?;
    socket_call(connection.as_mut(), method, params)
}

fn socket_call(connection: &mut dyn Connection, method: &str, params: Value) -> Result<Value, String> {
    let request = Request::new(format!("cli:{}", method.replace('.', ":")), method, params);
    match connection.call(&request) {
        Ok(Incoming::Success(response)) => Ok(response.result),
        Ok(Incoming::Error(response)) => Err(response.error.to_string()),
        Ok(Incoming::Event(_)) => Err("server returned an event instead of the matching response".to_owned()),
        Err(error) => Err(format!("server connection failed: {error}")),
    }
}

fn open_registry(
    paths: &starcil_platform::PlatformPaths,
    session: &str,
) -> Result<PluginRegistry, String> {
    let registry_file = paths.data_dir().join(format!("plugins-{session}.json"));
    let plugin_data_root = paths.data_dir().join("plugins");
    PluginRegistry::open_for_current_binary(RegistryPaths::new(registry_file, plugin_data_root))
        .map_err(|error| error.to_string())
}

fn local_executor(selection: &EndpointSelection) -> Result<PluginExecutor, String> {
    let binary = std::env::current_exe().map_err(|error| error.to_string())?;
    let host = HostEnvironment::for_current_platform(
        endpoint_for(selection).to_string_lossy().into_owned(),
        binary,
    );
    Ok(PluginExecutor::new(host, LogStore::new(256, 8 * 1024)))
}

fn plugin_from_result(result: &Value) -> Result<PluginEntry, String> {
    serde_json::from_value(
        result
            .get("plugin")
            .cloned()
            .ok_or_else(|| "plugin response omitted `plugin`".to_owned())?,
    )
    .map_err(|error| format!("invalid plugin response: {error}"))
}

fn render_plugin_list(plugins: Vec<PluginEntry>, filter: Option<&str>, json_output: bool) {
    let plugins = plugins
        .into_iter()
        .filter(|entry| filter.map_or(true, |filter| entry.plugin_id == filter))
        .collect::<Vec<_>>();
    if json_output {
        println!("{}", json!({"type": "plugin_list", "plugins": plugins}));
        return;
    }
    if plugins.is_empty() {
        println!("No plugins linked.");
        return;
    }
    println!("PLUGIN\tVERSION\tSTATE\tSOURCE");
    for entry in plugins {
        let source = match &entry.source {
            Some(SourceMetadata::Github { owner, repo, subdir, .. }) => match subdir {
                Some(subdir) => format!("{owner}/{repo}/{subdir}"),
                None => format!("{owner}/{repo}"),
            },
            None => entry.plugin_root.display().to_string(),
        };
        println!(
            "{}\t{}\t{}\t{}",
            entry.plugin_id,
            entry.version,
            if entry.enabled { "enabled" } else { "disabled" },
            source
        );
        for warning in entry.warnings {
            println!("  warning: {warning}");
        }
    }
}

fn render_action_list(result: Value) {
    let actions = result.get("actions").and_then(Value::as_array).cloned().unwrap_or_default();
    if actions.is_empty() {
        println!("No plugin actions available.");
        return;
    }
    println!("ACTION\tPLUGIN\tSTATE\tTITLE");
    for action in actions {
        println!(
            "{}\t{}\t{}\t{}",
            action.get("action_id").and_then(Value::as_str).unwrap_or("-"),
            action.get("plugin_id").and_then(Value::as_str).unwrap_or("-"),
            if action.get("enabled").and_then(Value::as_bool).unwrap_or(false) { "enabled" } else { "disabled" },
            action.get("title").and_then(Value::as_str).unwrap_or("-")
        );
    }
}

fn render_log_list(result: Value) {
    let logs = result.get("logs").and_then(Value::as_array).cloned().unwrap_or_default();
    if logs.is_empty() {
        println!("No plugin command logs available.");
        return;
    }
    for log in logs {
        println!("{}", log);
    }
}

fn print_plugin_result(action: &str, entry: &PluginEntry) {
    println!("Plugin {}: {} ({})", action, entry.plugin_id, entry.version);
}

fn qualify_action_id(action_id: &str, plugin_id: Option<&str>) -> String {
    match plugin_id {
        Some(plugin_id) if !action_id.starts_with(&format!("{plugin_id}.")) => format!("{plugin_id}.{action_id}"),
        _ => action_id.to_owned(),
    }
}

fn target_matches(target: &PluginTarget, entry: &PluginEntry) -> bool {
    match target {
        PluginTarget::PluginId(plugin_id) => entry.plugin_id == plugin_id.as_str(),
        PluginTarget::Github(slug) => matches!(
            &entry.source,
            Some(SourceMetadata::Github { owner, repo, subdir, .. })
                if owner == &slug.owner && repo == &slug.repo && subdir == &slug.subdir
        ),
    }
}

fn target_label(target: &PluginTarget) -> String {
    match target {
        PluginTarget::PluginId(plugin_id) => plugin_id.clone(),
        PluginTarget::Github(slug) => slug.as_str(),
    }
}

fn managed_github_root(paths: &starcil_platform::PlatformPaths) -> PathBuf {
    paths.data_dir().join("plugins").join("github")
}

fn absolute_existing_path(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_err(|error| error.to_string())?.join(path)
    };
    fs::canonicalize(&path).map_err(|error| format!("could not access {}: {error}", path.display()))
}

fn canonical_child(root: &Path, child: &Path) -> Result<PathBuf, String> {
    let root = fs::canonicalize(root).map_err(|error| format!("could not access {}: {error}", root.display()))?;
    let child = fs::canonicalize(child).map_err(|error| format!("could not access {}: {error}", child.display()))?;
    if child == root || child.starts_with(&root) {
        Ok(child)
    } else {
        Err(format!("plugin path {} escapes managed checkout {}", child.display(), root.display()))
    }
}

fn cleanup_new_checkout(root: &Path, checkout: &Path) -> Result<(), String> {
    if !checkout.exists() {
        return Ok(());
    }
    remove_managed_checkout(root, checkout)
}

fn remove_managed_checkout(root: &Path, checkout: &Path) -> Result<(), String> {
    let root = fs::canonicalize(root).map_err(|error| format!("could not access {}: {error}", root.display()))?;
    let checkout = fs::canonicalize(checkout)
        .map_err(|error| format!("could not access managed checkout {}: {error}", checkout.display()))?;
    if checkout == root || !checkout.starts_with(&root) {
        return Err(format!("refusing to delete unmanaged path {}", checkout.display()));
    }
    fs::remove_dir_all(&checkout).map_err(|error| format!("could not delete {}: {error}", checkout.display()))
}

fn run_git<const N: usize>(args: [&str; N]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn git_stdout<const N: usize>(args: [&str; N]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn plugin_error(error: impl std::fmt::Display) -> i32 {
    eprintln!("starcil: {error}");
    1
}
