//! stdio servers inherit the parent environment without credential-like
//! names (dsh `scrubbedParentEnv`); configured `env` entries and the names
//! in `env_vars` always pass.
mod common;

use common::*;
use serde_json::{Value, json};

#[test]
fn credentials_stay_out_of_stdio_servers() {
    // SAFETY: the only test of this binary sets the variables before any
    // other thread (runtime, servers) exists.
    unsafe {
        std::env::set_var("FAKE_API_KEY", "secret");
        std::env::set_var("PATH_PROBE", "yes");
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let config = config(vec![
            ("plain", stdio_server(&[("MCP_TEST_VAR", "v")])),
            ("explicit", stdio_server(&[("FAKE_API_KEY", "configured")])),
            (
                "passthrough",
                dshx_core::config::McpServerConfig {
                    env_vars: vec!["FAKE_API_KEY".into(), "UNSET_PROBE".into()],
                    ..stdio_server(&[])
                },
            ),
        ]);
        let (tools, mcp) = dshx_mcp::mcp_tools(&config).await.unwrap();
        let env = |name: &'static str| {
            let tools = tools.clone();
            async move {
                let parts = call(&tools, name, json!({}), ctx()).await.1;
                serde_json::from_str::<Value>(&only_text(&parts)).unwrap()["env"].clone()
            }
        };
        let plain = env("mcp__plain__info").await;
        assert_eq!(plain["FAKE_API_KEY"], Value::Null);
        assert_eq!(plain["PATH_PROBE"], "yes");
        assert_eq!(plain["MCP_TEST_VAR"], "v");
        let explicit = env("mcp__explicit__info").await;
        assert_eq!(explicit["FAKE_API_KEY"], "configured");
        // `env_vars` lets a credential-like name through on purpose.
        let passed = env("mcp__passthrough__info").await;
        assert_eq!(passed["FAKE_API_KEY"], "secret");
        mcp.shutdown().await;
    });
}
