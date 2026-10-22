// "Paste JSON" of the Add server dialog: the formats ui/src/mcp-import.js
// reads. Run with `node ui/dev/mcp_import_check.mjs` (no dependencies).
import { parseServers } from '../src/mcp-import.js';
import assert from 'node:assert/strict';
const cases = {
  claude: `{"mcpServers": {"gh": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-gh"], "env": {"GH_PERSONAL_ACCESS_TOKEN": "ghp_x"}}, "linear": {"url": "https://mcp.linear.app/mcp"}}}`,
  vscode: `// settings\n{"servers": {"fetch": {"type": "stdio", "command": "uvx", "args": ["mcp-server-fetch"],},},}`,
  fragment: `"filesystem": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]}`,
  bare: `{"command": "docker run -i --rm mcp/time", "env": {"TOKEN": "\${TOKEN}"}}`,
  toml: `[mcp_servers.docs]\ncommand = "npx"\nargs = ["-y", "mcp-docs"]\nenv = { "API_KEY" = "k" }\n[mcp_servers.remote]\nurl = "https://x.example/mcp"\nbearer_token_env_var = "X_TOKEN"\n`,
  fence: "Add this:\n```json\n{\"mcpServers\":{\"a\":{\"url\":\"https://a.example/sse\",\"headers\":{\"Authorization\":\"Bearer abc\"}}}}\n```",
  url: 'https://mcp.notion.com/mcp',
  cmd: 'npx -y @upstash/context7-mcp',
  junk: '{hello',
};
const r = Object.fromEntries(Object.entries(cases).map(([k, v]) => [k, parseServers(v)]));
assert.equal(r.claude.servers.length, 2);
assert.deepEqual(r.claude.servers[0].form.env, [{ key: 'GH_PERSONAL_ACCESS_TOKEN', value: 'ghp_x' }]);
assert.equal(r.claude.servers[1].form.type, 'http');
assert.equal(r.claude.servers[1].form.auth, 'oauth');
assert.equal(r.vscode.servers[0].form.command, 'uvx');
assert.equal(r.fragment.servers[0].form.name, 'filesystem');
assert.equal(r.bare.servers[0].form.command, 'docker');
assert.deepEqual(r.bare.servers[0].form.args, ['run', '-i', '--rm', 'mcp/time']);
assert.deepEqual(r.bare.servers[0].form.envVars, ['TOKEN']);
assert.equal(r.toml.servers.length, 2);
assert.deepEqual(r.toml.servers[0].form.env, [{ key: 'API_KEY', value: 'k' }]);
assert.equal(r.toml.servers[1].form.bearerEnv, 'X_TOKEN');
assert.equal(r.fence.servers[0].form.bearerToken, 'abc');
assert.ok(r.fence.servers[0].warnings.includes('sse'));
assert.equal(r.url.servers[0].form.name, 'notion');
assert.equal(r.junk.error, 'unreadable');
assert.deepEqual(r.cmd.servers[0].form.args, ['-y', '@upstash/context7-mcp']);
console.log('mcp-import: all formats read');
