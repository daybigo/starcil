use serde_json::{json, Value};
use starcil_integrations::{ClaudeIntegration, CodexIntegration, Integration};
use std::fs;

#[test]
fn real_integrations_round_trip_only_inside_injected_temp_home() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();
    fs::create_dir(home.path().join(".codex")).unwrap();

    let claude_settings = home.path().join(".claude/settings.json");
    fs::write(
        &claude_settings,
        serde_json::to_vec_pretty(&json!({
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type":"command", "command":"user-stop-hook"}]
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let claude = ClaudeIntegration;
    assert!(claude.install(home.path()).unwrap().changed);
    let installed_bytes = fs::read(&claude_settings).unwrap();
    assert!(!claude.install(home.path()).unwrap().changed);
    assert_eq!(fs::read(&claude_settings).unwrap(), installed_bytes);
    assert!(claude.status(home.path()).unwrap().installed);
    claude.uninstall(home.path()).unwrap();
    let after: Value = serde_json::from_slice(&fs::read(&claude_settings).unwrap()).unwrap();
    assert_eq!(after["hooks"]["Stop"][0]["hooks"][0]["command"], "user-stop-hook");

    let codex_config = home.path().join(".codex/config.toml");
    let original_codex = "# retain me\nmodel = \"gpt-5\"\n";
    fs::write(&codex_config, original_codex).unwrap();
    let codex = CodexIntegration;
    assert!(codex.install(home.path()).unwrap().changed);
    assert!(codex.status(home.path()).unwrap().installed);
    codex.uninstall(home.path()).unwrap();
    assert_eq!(fs::read_to_string(codex_config).unwrap(), original_codex);
}
