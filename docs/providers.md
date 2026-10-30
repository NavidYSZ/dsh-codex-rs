# Providers transferred from the original DSH

Catalog snapshot: 28 October 2026. Route ids, model ids, context sizes,
modalities and supported efforts follow the existing DSH desktop profile
and its installed pi-ai catalog. Provider credentials are personal state;
they are never included in the repository or a build.

| Route | Endpoint | Models from the original profile |
|---|---|---|
| `opencode-go` | `https://opencode.ai/zen/go/v1` | `deepseek-v4.1-flash`, `glm-5.3`, `glm-5.3-flash`, `kimi-k3` |
| `xiaomi-token-plan-ams` | `https://token-plan-ams.xiaomimimo.com/v1` | `mimo-v2.6-pro` |
| `zai` | `https://api.z.ai/api/coding/paas/v4` | `glm-5.3`, `glm-5.3-flash`, `glm-5.3-highspeed` |
| `command-code` | `https://api.commandcode.ai/provider/v1` | `deepseek/deepseek-v4.1-flash`, `deepseek/deepseek-v4.1-flash-fast`, `moonshotai/Kimi-K3`, `z-ai/glm-5.3-flashx`, `xiaomi/mimo-v2.6-pro-ultraspeed` |
| `anthropic` | `https://api.anthropic.com` | `claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5` |

The Anthropic catalog also offers Claude Fable 5.1; importing this profile
retains its explicitly chosen three models. Direct DeepSeek stays on its
existing route and key. No default model is changed by importing providers.

Chat Completions models may set `thinking_format = "deepseek"` (explicit
`thinking.type`) or `"zai"` (also `clear_thinking = false`). Without that
field, their effort is sent as `reasoning_effort`. A route can set
`[providers.<id>.compat] reasoning_content = true` to preserve same-model
reasoning between tool calls, and `tool_stream = true` for Z.ai. Models
inherit their route's catalog metadata when edited or imported; the UI keeps
the format when a model row is saved. Unsupported efforts resolve to the
nearest supported level (e.g. MiMo `max` → `high`, GLM `off` → `low`).

OpenCode Go requests include our own user agent and a stable
`x-opencode-session` for each conversation. Auxiliary requests without a
session id use a stable client-local fallback. This implements
[OpenCode's routing requirements](https://dev.opencode.ai/v2/docs/console/go/#where-can-i-use-it).
GLM's mandatory thinking and preservation follow the
[Z.ai thinking documentation](https://docs.z.ai/guides/capabilities/thinking-mode).
Command Code's configured models use the documented
[Chat Completions endpoint](https://commandcode.ai/docs/provider).

Catalog token prices are a snapshot of the original installed catalog.
Models without known prices (including subscription-only MiMo and GLM
Highspeed) show token counts without inventing a zero price. Subscription
access and remaining quota are determined by the provider.

Offline checks cover catalog resolution, correct endpoint/auth/session
headers, thinking parameters, reasoning replay after tools and provider
switches, streaming, usage and the existing web settings flow. The opt-in
`provider_smoke` example accepts `[{"provider","config","key"}]` on stdin
and runs one in-memory echo tool plus a follow-up request per selected model.
It prints only test outcomes and token usage; keys and reasoning are withheld.

The import script parses YAML without executing `!!js`, preserves YAML 1.2
effort names such as `off`, refuses redirects, and copies keys through the
authenticated destination API. Use `--backup-home` for the destination's
local home directory to save protected config/credential copies first.
