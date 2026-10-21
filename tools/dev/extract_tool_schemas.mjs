// Prints the compiled model-facing tool definitions and prompt sections of
// dsh's goal, schedule and agent-team plugins as JSON (the goldens under
// docs/rust/upstream/tools-0.2.0-rc.2/). Run from the installed dsh package:
//   cd /opt/homebrew/lib/node_modules/@deepseek-ai/dsh && node <this file>
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const require = createRequire(process.cwd() + "/package.json");
const load = async (name) => import(pathToFileURL(require.resolve(name)).href);
const goal = await load("@deepseek-ai/dsh-tool-goal");
const schedule = await load("@deepseek-ai/dsh-schedule");
const team = await load("@deepseek-ai/dsh-experimental-tool-agent-team");
const { SECTION_ORDERS } = await import(pathToFileURL(require.resolve("@deepseek-ai/dsh-system-prompt")).href).then((m) => ({ SECTION_ORDERS: m.SECTION_ORDERS }));

const ORDERS = { TOOL_GOAL: 2400, TEAM_POLICY: 600 };
function recorder() {
  const tools = [];
  const sections = [];
  return {
    tools,
    sections,
    ctx: {
      tools: { register(def) { tools.push(def); return () => {}; } },
      systemPrompt: {
        section(s) { sections.push(s); return () => {}; },
        getSectionOrder(name) { return ORDERS[name] ?? SECTION_ORDERS?.[name]; },
      },
    },
  };
}
const view = (def) => ({ name: def.name, description: def.description, parameters: def.parameters, output: def.output.schema });

const g = recorder();
goal.apply({ ...g.ctx, agents: {}, goals: {}, sessionProjections: {} }, {});

const s = recorder();
const agent = { session: { id: "s" } };
schedule.registerScheduleTools({}, s.ctx, agent);

const t = recorder();
const lead = { id: "lead", ctx: t.ctx };
team.apply({
  agents: { list: () => [lead] },
  agentTeams: { tryMembership: () => ({ role: "lead" }) },
  on() { return () => {}; },
  effect() {},
}, {});

console.log(JSON.stringify({
  goal: { tools: g.tools.map(view), sections: g.sections },
  schedule: { tools: s.tools.map(view), sections: s.sections },
  team: { tools: t.tools.map(view), sections: t.sections },
}, null, 2));
