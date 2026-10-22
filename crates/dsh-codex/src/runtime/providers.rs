//! Model providers of a runtime: the built-in DeepSeek route plus every
//! `[providers.<id>]` table behind one [`ProviderRouter`], their API keys
//! (environment first, then `credentials.toml`), the model list of
//! `/api/info`, and the Models settings routes ([`RuntimeProviders`]).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use async_trait::async_trait;
use dshx_core::config::{Config, ModelCost, ProviderConfig};
use dshx_core::{AttachmentStore, ImageAttachmentRef, LlmClient};
use dshx_llm::discover::{Probe, fetch_models};
use dshx_llm::provider::{catalog, catalog_entry, check_base_url};
use dshx_llm::{
    ApiKeyChain, ApiKeySource, DeepSeekClient, DeepSeekConfig, EnvApiKey, PROVIDER, ProviderClient,
    ProviderModel, ProviderSpec, SharedApiKey, model_ref,
};
use dshx_server::{NotFound, Providers};
use serde_json::{Map, Value, json};

use super::Runtime;

/// Keys stored per provider route, shared with that route's client so a
/// new key works at the next request.
#[derive(Default)]
pub(crate) struct ProviderKeys {
    stored: Mutex<HashMap<String, SharedApiKey>>,
    /// Serializes config and key changes.
    ops: tokio::sync::Mutex<()>,
}

/// A non-empty environment variable.
fn env_value(var: Option<&str>) -> Option<String> {
    var.and_then(|v| std::env::var(v).ok())
        .filter(|v| !v.trim().is_empty())
}

impl Runtime {
    /// The stored key of a provider route, loaded once from `credentials.toml`.
    fn stored_key(&self, provider: &str) -> SharedApiKey {
        let mut stored = self.provider_keys.stored.lock().unwrap();
        stored
            .entry(provider.to_owned())
            .or_insert_with(|| {
                SharedApiKey::new(self.credentials.provider_key(provider).ok().flatten())
            })
            .clone()
    }

    fn key_source(&self, spec: &ProviderSpec) -> Arc<dyn ApiKeySource> {
        let mut chain: Vec<Arc<dyn ApiKeySource>> = Vec::new();
        if let Some(var) = &spec.api_key_env {
            chain.push(Arc::new(EnvApiKey { var: var.clone() }));
        }
        chain.push(Arc::new(self.stored_key(&spec.id)));
        Arc::new(ApiKeyChain(chain))
    }

    /// Valid provider routes of `config`; a broken table is skipped (and
    /// shown with its error on the Models page).
    fn specs(config: &Config) -> Vec<ProviderSpec> {
        config
            .providers
            .iter()
            .filter_map(|(id, table)| match ProviderSpec::resolve(id, table) {
                Ok(spec) => Some(spec),
                Err(e) => {
                    tracing::warn!("{e}; the provider is skipped");
                    None
                }
            })
            .collect()
    }

    /// Rebuilds every provider route from the current config.
    pub(crate) fn apply_providers(&self) {
        let config = self.config();
        let store = self.attachments.clone();
        let resolver_store = self.attachments.clone();
        let image_path: Arc<dshx_llm::images::ImagePathResolver> =
            Arc::new(move |image: &ImageAttachmentRef| {
                resolver_store
                    .object_path(&image.attachment_id)
                    .ok()
                    .map(|p| p.display().to_string())
            });
        let clients = Self::specs(&config)
            .into_iter()
            .filter_map(|spec| {
                let key = self.key_source(&spec);
                ProviderClient::new(
                    spec,
                    key,
                    Some(store.clone() as Arc<dyn AttachmentStore>),
                    Some(image_path.clone()),
                )
                .inspect_err(|e| tracing::warn!("provider route: {e}"))
                .ok()
            })
            .collect();
        self.router.set_routes(clients);
    }

    /// `(provider, model id)` of a model reference.
    pub fn split_model(&self, reference: &str) -> (String, String) {
        self.router.split(reference)
    }

    /// Why a prompt for `reference` cannot be sent, if it cannot.
    pub fn model_ready(&self, reference: &str) -> Result<(), String> {
        let (provider, _) = self.split_model(reference);
        if provider == PROVIDER {
            return if self.api_key_configured() {
                Ok(())
            } else {
                Err("No DeepSeek API key yet. Add it in Settings → Models.".into())
            };
        }
        let Some(client) = self.router.route(&provider) else {
            return Err(format!(
                "The model provider \"{provider}\" is not configured. Pick another model or add it in Settings → Models."
            ));
        };
        if self.key_status(client.spec())["configured"] == true || !client.spec().key_required {
            Ok(())
        } else {
            Err(format!(
                "No API key for {} yet. Add it in Settings → Models.",
                client.spec().name
            ))
        }
    }

    /// Whether any route can take a prompt.
    pub fn any_model_ready(&self) -> bool {
        self.api_key_configured()
            || self
                .router
                .routes()
                .iter()
                .any(|c| !c.spec().key_required || self.key_status(c.spec())["configured"] == true)
    }

    fn key_status(&self, spec: &ProviderSpec) -> Value {
        let env = env_value(spec.api_key_env.as_deref());
        let stored = self.stored_key(&spec.id).get().is_some();
        key_json(
            env.is_some(),
            stored,
            spec.api_key_env.as_deref(),
            spec.key_required,
        )
    }

    fn deepseek_key_status(&self) -> Value {
        let env = env_value(Some(dshx_llm::API_KEY_ENV)).is_some();
        let stored = self.credentials.api_key().ok().flatten().is_some();
        key_json(env, stored, Some(dshx_llm::API_KEY_ENV), true)
    }

    /// `/api/info` models (one row per model reference, DeepSeek first) and
    /// the provider groups they belong to.
    pub fn info_models(&self) -> (Vec<Value>, Vec<Value>) {
        let mut models = Vec::new();
        let mut groups = Vec::new();
        let deepseek = self.router.deepseek();
        let ready = self.api_key_configured();
        groups.push(json!({"id": PROVIDER, "name": "DeepSeek", "available": ready}));
        for info in deepseek.models() {
            let resolved = deepseek.resolve_model(&info.id);
            let mut row = serde_json::to_value(&info).unwrap_or_default();
            row["provider"] = PROVIDER.into();
            row["providerName"] = "DeepSeek".into();
            row["model"] = info.id.clone().into();
            row["maxTokens"] = resolved.default_max_tokens.into();
            row["available"] = ready.into();
            models.push(row);
        }
        for client in self.router.routes() {
            let spec = client.spec();
            let ready = !spec.key_required || self.key_status(spec)["configured"] == true;
            groups.push(json!({"id": spec.id, "name": spec.name, "available": ready}));
            for model in &spec.models {
                let mut row = serde_json::to_value(model.info()).unwrap_or_default();
                row["id"] = model_ref(&spec.id, &model.id).into();
                row["provider"] = spec.id.clone().into();
                row["providerName"] = spec.name.clone().into();
                row["model"] = model.id.clone().into();
                row["maxTokens"] = model.max_tokens.into();
                if let Some(cost) = &model.cost {
                    row["cost"] = cost_json(cost);
                }
                row["available"] = ready.into();
                models.push(row);
            }
        }
        (models, groups)
    }

    /// The Models page row of one provider.
    fn provider_json(&self, id: &str) -> Option<Value> {
        let config = self.config();
        if id == PROVIDER {
            return Some(self.deepseek_json(&config));
        }
        let table = config.providers.get(id)?;
        Some(match ProviderSpec::resolve(id, table) {
            Ok(spec) => spec_json(&spec, self.key_status(&spec)),
            Err(e) => json!({
                "id": id,
                "name": table.display_name.clone().unwrap_or_else(|| id.to_owned()),
                "api": table.api,
                "baseUrl": table.base_url,
                "builtin": false,
                "catalog": catalog_entry(id).is_some(),
                "error": e.0,
                "key": key_json(env_value(table.api_key_env.as_deref()).is_some(),
                    self.stored_key(id).get().is_some(), table.api_key_env.as_deref(), false),
                "models": [],
            }),
        })
    }

    fn deepseek_json(&self, config: &Config) -> Value {
        let deepseek = self.router.deepseek();
        let base_env = env_value(Some(dshx_llm::BASE_URL_ENV));
        let models: Vec<Value> = deepseek
            .models()
            .into_iter()
            .map(|info| {
                let resolved = deepseek.resolve_model(&info.id);
                json!({
                    "id": info.id, "ref": info.id, "name": info.name,
                    "contextWindow": info.context_window, "maxTokens": resolved.default_max_tokens,
                    "input": info.input_modalities, "reasoningEfforts": info.reasoning_efforts,
                    "defaultReasoningEffort": info.default_reasoning_effort,
                })
            })
            .collect();
        json!({
            "id": PROVIDER,
            "name": "DeepSeek",
            "api": "anthropic-messages",
            "baseUrl": base_env.clone().unwrap_or_else(|| config.base_url.clone()),
            "defaultBaseUrl": dshx_llm::PUBLIC_BASE_URL,
            "baseUrlLocked": base_env.is_some(),
            "apiKeyEnv": dshx_llm::API_KEY_ENV,
            "builtin": true,
            "catalog": true,
            "modelsEditable": false,
            "modelsCustomized": false,
            "key": self.deepseek_key_status(),
            "headers": [],
            "models": models,
        })
    }

    fn rebuild_deepseek(&self) -> anyhow::Result<()> {
        let config = self.config();
        let resolver_store = self.attachments.clone();
        let client = DeepSeekClient::builder(DeepSeekConfig::from_core(&config))
            .api_key(Arc::new(self.api_key.clone()))
            .attachments(self.attachments.clone() as Arc<dyn AttachmentStore>)
            .image_path(Arc::new(move |image: &ImageAttachmentRef| {
                resolver_store
                    .object_path(&image.attachment_id)
                    .ok()
                    .map(|p| p.display().to_string())
            }))
            .build()
            .map_err(|e| anyhow::anyhow!("DeepSeek client: {e}"))?;
        self.router.set_deepseek(Arc::new(client));
        Ok(())
    }
}

fn key_json(env: bool, stored: bool, var: Option<&str>, required: bool) -> Value {
    json!({
        "configured": env || stored,
        "source": if env { Some("env") } else if stored { Some("stored") } else { None },
        "locked": env,
        "env": var,
        "required": required,
    })
}

/// `{input, output, cacheRead, cacheWrite}` in USD per million tokens.
pub fn cost_json(cost: &ModelCost) -> Value {
    json!({
        "input": cost.input, "output": cost.output,
        "cacheRead": cost.cache_read, "cacheWrite": cost.cache_write,
    })
}

fn model_json(provider: &str, model: &ProviderModel) -> Value {
    let mut row = json!({
        "id": model.id,
        "ref": model_ref(provider, &model.id),
        "name": model.name,
        "contextWindow": model.context_window,
        "maxTokens": model.max_tokens,
        "input": model.input,
        "reasoningEfforts": model.efforts,
        "defaultReasoningEffort": model.default_effort,
        "thinking": model.thinking.as_str(),
    });
    if let Some(cost) = &model.cost {
        row["cost"] = cost_json(cost);
    }
    row
}

fn spec_json(spec: &ProviderSpec, key: Value) -> Value {
    let entry = catalog_entry(&spec.id);
    json!({
        "id": spec.id,
        "name": spec.name,
        "api": spec.api.as_str(),
        "baseUrl": spec.base_url,
        "defaultBaseUrl": entry.as_ref().map(|e| e.base_url.clone()),
        "apiKeyEnv": spec.api_key_env,
        "builtin": false,
        "catalog": spec.catalog,
        "modelsEditable": true,
        "modelsCustomized": spec.models_customized,
        "titleModel": spec.title_model,
        "headers": spec.headers.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        "compat": {
            "auth": if spec.compat.bearer { "bearer" } else { "x-api-key" },
            "cacheControl": spec.compat.cache_control,
            "maxTokensField": spec.compat.max_tokens_field,
            "reasoningContent": spec.compat.reasoning_content,
        },
        "key": key,
        "models": spec.models.iter().map(|m| model_json(&spec.id, m)).collect::<Vec<_>>(),
    })
}

/// Drops `null` members (a cleared field), at the top and one level down.
fn without_nulls(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        map.retain(|_, v| !v.is_null());
        for v in map.values_mut() {
            if let Some(inner) = v.as_object_mut() {
                inner.retain(|_, v| !v.is_null());
            }
        }
    }
    value
}

/// The Models page routes over a runtime.
pub struct RuntimeProviders {
    rt: Arc<Runtime>,
}

impl RuntimeProviders {
    pub fn new(rt: Arc<Runtime>) -> Self {
        RuntimeProviders { rt }
    }

    fn row(&self, id: &str) -> anyhow::Result<Value> {
        self.rt
            .provider_json(id)
            .ok_or_else(|| NotFound(format!("provider {id}")).into())
    }

    fn save_deepseek(&self, body: &Value) -> anyhow::Result<Value> {
        if let Some(url) = body.get("baseUrl").and_then(Value::as_str) {
            let url = url.trim();
            let url = if url.is_empty() {
                dshx_llm::PUBLIC_BASE_URL
            } else {
                url
            };
            check_base_url(url).map_err(|e| anyhow::anyhow!("{}", e.0))?;
            self.rt.update_config(|c| c.base_url = url.to_owned())?;
            self.rt.rebuild_deepseek()?;
        }
        if let Some(key) = body
            .get("apiKey")
            .and_then(Value::as_str)
            .filter(|k| !k.trim().is_empty())
        {
            anyhow::ensure!(
                env_value(Some(dshx_llm::API_KEY_ENV)).is_none(),
                "the DeepSeek key comes from {} (read-only)",
                dshx_llm::API_KEY_ENV
            );
            self.rt.set_api_key(Some(key))?;
        }
        self.row(PROVIDER)
    }

    fn store_key(
        &self,
        spec_id: &str,
        env: Option<&str>,
        name: &str,
        key: Option<&str>,
    ) -> anyhow::Result<()> {
        if let Some(var) = env
            && env_value(Some(var)).is_some()
        {
            anyhow::bail!("{name}'s key comes from {var} (read-only)");
        }
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(key) => {
                anyhow::ensure!(
                    key.bytes().all(|b| (0x21..=0x7e).contains(&b)),
                    "This API key is not in a valid format. Please check it."
                );
                self.rt.credentials.set_provider_key(spec_id, key)?;
                self.rt.stored_key(spec_id).set(Some(key.to_owned()));
            }
            None => {
                self.rt.credentials.clear_provider_key(spec_id)?;
                self.rt.stored_key(spec_id).set(None);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Providers for RuntimeProviders {
    async fn list(&self) -> anyhow::Result<Value> {
        let config = self.rt.config();
        let mut rows = vec![self.rt.deepseek_json(&config)];
        rows.extend(
            config
                .providers
                .keys()
                .filter_map(|id| self.rt.provider_json(id)),
        );
        let mut entries = vec![json!({
            "id": PROVIDER, "name": "DeepSeek", "api": "anthropic-messages",
            "baseUrl": dshx_llm::PUBLIC_BASE_URL, "apiKeyEnv": dshx_llm::API_KEY_ENV,
            "models": 2, "added": true,
        })];
        for entry in catalog() {
            entries.push(json!({
                "id": entry.id, "name": entry.name, "api": entry.api.as_str(),
                "baseUrl": entry.base_url, "apiKeyEnv": entry.api_key_env,
                "models": entry.models.len(),
                "added": config.providers.contains_key(&entry.id),
            }));
        }
        Ok(json!({"providers": rows, "catalog": entries}))
    }

    async fn save(&self, id: &str, body: Value) -> anyhow::Result<Value> {
        let _ops = self.rt.provider_keys.ops.lock().await;
        if id == PROVIDER {
            return self.save_deepseek(&body);
        }
        let config = self.rt.config();
        let existing = config.providers.get(id).cloned();
        let body = without_nulls(body);
        let mut table: ProviderConfig =
            serde_json::from_value(body.clone()).context("invalid provider settings")?;
        // Header values are never sent to the page; without new ones keep the old.
        if body.get("headers").is_none() {
            table.headers = existing
                .as_ref()
                .map(|e| e.headers.clone())
                .unwrap_or_default();
        }
        // The catalog's own models stay inherited, so its updates arrive.
        if let Some(entry) = catalog_entry(id)
            && !table.models.is_empty()
        {
            let resolved =
                ProviderSpec::resolve(id, &table).map_err(|e| anyhow::anyhow!("{}", e.0))?;
            if resolved.models == entry.models {
                table.models.clear();
            }
        }
        let spec = ProviderSpec::resolve(id, &table).map_err(|e| anyhow::anyhow!("{}", e.0))?;
        if let Some(key) = body
            .get("apiKey")
            .and_then(Value::as_str)
            .filter(|k| !k.trim().is_empty())
        {
            self.store_key(id, spec.api_key_env.as_deref(), &spec.name, Some(key))?;
        }
        self.rt.update_config(|c| {
            c.providers.insert(id.to_owned(), table);
        })?;
        self.rt.apply_providers();
        self.row(id)
    }

    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        let _ops = self.rt.provider_keys.ops.lock().await;
        anyhow::ensure!(id != PROVIDER, "DeepSeek is built in and cannot be removed");
        let config = self.rt.config();
        if !config.providers.contains_key(id) {
            return Err(NotFound(format!("provider {id}")).into());
        }
        let (default_provider, _) = self.rt.split_model(&config.model);
        self.rt.update_config(|c| {
            c.providers.remove(id);
            // The default model must stay sendable.
            if default_provider == id {
                c.model = Config::default().model;
            }
        })?;
        self.rt.credentials.clear_provider_key(id)?;
        self.rt.provider_keys.stored.lock().unwrap().remove(id);
        self.rt.apply_providers();
        Ok(())
    }

    async fn set_key(&self, id: &str, key: Option<String>) -> anyhow::Result<Value> {
        let _ops = self.rt.provider_keys.ops.lock().await;
        if id == PROVIDER {
            anyhow::ensure!(
                env_value(Some(dshx_llm::API_KEY_ENV)).is_none(),
                "the DeepSeek key comes from {} (read-only)",
                dshx_llm::API_KEY_ENV
            );
            self.rt.set_api_key(key.as_deref())?;
            return self.row(PROVIDER);
        }
        let config = self.rt.config();
        let table = config
            .providers
            .get(id)
            .ok_or_else(|| NotFound(format!("provider {id}")))?;
        let env = table
            .api_key_env
            .clone()
            .or_else(|| catalog_entry(id).and_then(|e| e.api_key_env));
        let name = table.display_name.clone().unwrap_or_else(|| id.to_owned());
        self.store_key(id, env.as_deref(), &name, key.as_deref())?;
        self.row(id)
    }

    async fn fetch_models(&self, id: &str, probe: Value) -> anyhow::Result<Value> {
        anyhow::ensure!(id != PROVIDER, "DeepSeek's models are built in");
        let text = |name: &str| {
            probe
                .get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let config = self.rt.config();
        let spec = config
            .providers
            .get(id)
            .and_then(|t| ProviderSpec::resolve(id, t).ok());
        let entry = catalog_entry(id);
        let api = match text("api") {
            Some(api) => dshx_llm::Api::parse(&api)
                .ok_or_else(|| anyhow::anyhow!("unknown API protocol {api:?}"))?,
            None => spec
                .as_ref()
                .map(|s| s.api)
                .or(entry.as_ref().map(|e| e.api))
                .ok_or_else(|| anyhow::anyhow!("Choose the API protocol first."))?,
        };
        let base_url = text("baseUrl")
            .or_else(|| spec.as_ref().map(|s| s.base_url.clone()))
            .or_else(|| entry.as_ref().map(|e| e.base_url.clone()))
            .ok_or_else(|| anyhow::anyhow!("Enter the base URL first, then fetch."))?;
        check_base_url(&base_url).map_err(|e| anyhow::anyhow!("{}", e.0))?;
        let key = match text("apiKey") {
            Some(key) => Some(key),
            None => match &spec {
                Some(spec) => self.rt.key_source(spec).api_key().await,
                None => env_value(entry.as_ref().and_then(|e| e.api_key_env.as_deref())),
            },
        };
        let bearer = spec
            .as_ref()
            .map(|s| s.compat.bearer)
            .unwrap_or(api == dshx_llm::Api::OpenAiCompletions);
        let headers = spec.as_ref().map(|s| s.headers.clone()).unwrap_or_default();
        let models = fetch_models(&Probe {
            api,
            base_url: &base_url,
            key: key.as_deref(),
            bearer,
            headers: &headers,
        })
        .await
        .map_err(|f| anyhow::anyhow!("{}", f.message))?;
        let rows: Vec<Value> = models
            .iter()
            .map(|m| {
                let mut row = serde_json::to_value(m).unwrap_or_default();
                if let Some(cost) = &m.cost {
                    row["cost"] = cost_json(cost);
                }
                row
            })
            .collect();
        let mut out = Map::new();
        out.insert("models".into(), rows.into());
        Ok(Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleared_fields_are_dropped_before_parsing() {
        let body = without_nulls(json!({"models": null, "compat": {"auth": null}, "baseUrl": "x"}));
        assert_eq!(body, json!({"compat": {}, "baseUrl": "x"}));
        let table: ProviderConfig = serde_json::from_value(body).unwrap();
        assert_eq!(table.base_url.as_deref(), Some("x"));
    }

    #[test]
    fn keys_are_reported_without_values() {
        let v = key_json(true, true, Some("ANTHROPIC_API_KEY"), true);
        assert_eq!(
            v,
            json!({"configured": true, "source": "env", "locked": true, "env": "ANTHROPIC_API_KEY", "required": true})
        );
    }
}
