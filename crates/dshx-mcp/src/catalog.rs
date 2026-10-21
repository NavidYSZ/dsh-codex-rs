//! Built-in catalog of remote MCP servers ("connectors") the UI offers to
//! add with one click. All speak Streamable HTTP; `auth` says how to log in:
//! `oauth` (discovery and dynamic registration), `token` (personal access
//! token or an own OAuth client) or `none`.
use serde::{Deserialize, Serialize};

/// One catalog entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connector {
    /// Default server name.
    pub id: String,
    pub name: String,
    pub description: String,
    pub url: String,
    pub auth: String,
    pub category: String,
    pub website: String,
    pub brand_color: String,
    /// Where to create an access token (`auth = token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_help: Option<String>,
}

/// The catalog, in display order.
pub fn connectors() -> Vec<Connector> {
    serde_json::from_str(include_str!("connectors.json")).expect("valid connectors.json")
}

/// The entry with this id.
pub fn connector(id: &str) -> Option<Connector> {
    connectors().into_iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_valid() {
        let all = connectors();
        assert_eq!(all.len(), 15);
        for c in &all {
            assert!(crate::is_server_name(&c.id), "{}", c.id);
            assert!(c.url.starts_with("https://"), "{}", c.url);
            assert!(
                matches!(c.auth.as_str(), "oauth" | "token" | "none"),
                "{}",
                c.id
            );
        }
        assert_eq!(connector("github").unwrap().auth, "token");
        assert_eq!(
            connector("atlassian").unwrap().url,
            "https://mcp.atlassian.com/v1/mcp/authv2"
        );
    }
}
