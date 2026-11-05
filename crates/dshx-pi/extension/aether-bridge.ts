// Aether's bridge inside the Pi process (the only extension Aether loads).
// Embedded in dsh-codex and written to $DSHX_HOME/pi/ext/aether-bridge-<sha8>.ts.
//
// - Connects to the Unix socket in AETHER_BRIDGE with AETHER_BRIDGE_TOKEN and
//   deletes both variables from process.env at once.
// - Registers Aether's model providers; their keys arrive only in the
//   bridge's `config`/`providers` messages and stay in memory.
// - Replaces `bash` with Pi's own implementation whose environment drops
//   every variable naming a KEY, TOKEN, SECRET or PASSWORD and all AETHER_*.
// - Registers Aether's tools (MVP: todo_write); calls run in Aether.
// - Gates write, edit and the bridge tools through Aether's sandbox rules.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { createBashToolDefinition } from "@earendil-works/pi-coding-agent";
import { connect, type Socket } from "node:net";

const SECRET = /KEY|TOKEN|SECRET|PASSWORD/i;
const GATED = new Set(["write", "edit"]);

function filtered(env: Record<string, string | undefined>): Record<string, string> {
	const out: Record<string, string> = {};
	for (const [key, value] of Object.entries(env)) {
		if (value === undefined || SECRET.test(key) || key.startsWith("AETHER_")) continue;
		out[key] = value;
	}
	return out;
}

class Bridge {
	private buffer = "";
	private next = 1;
	private waiting = new Map<number, (message: any) => void>();
	onPush: (message: any) => void = () => {};

	private constructor(private socket: Socket) {
		socket.setEncoding("utf8");
		socket.on("data", (chunk: string) => {
			this.buffer += chunk;
			let at: number;
			while ((at = this.buffer.indexOf("\n")) >= 0) {
				const line = this.buffer.slice(0, at).replace(/\r$/, "");
				this.buffer = this.buffer.slice(at + 1);
				if (!line) continue;
				let message: any;
				try {
					message = JSON.parse(line);
				} catch {
					continue;
				}
				const reply = typeof message.id === "number" ? this.waiting.get(message.id) : undefined;
				if (reply && message.type !== "config") {
					this.waiting.delete(message.id);
					reply(message);
				} else if (message.type === "config" && this.waiting.has(0)) {
					const done = this.waiting.get(0)!;
					this.waiting.delete(0);
					done(message);
				} else {
					this.onPush(message);
				}
			}
		});
		const fail = () => {
			for (const [, reply] of this.waiting) reply({ type: "closed" });
			this.waiting.clear();
		};
		socket.on("close", fail);
		socket.on("error", fail);
	}

	static open(path: string): Promise<Bridge> {
		return new Promise((resolve, reject) => {
			const socket = connect(path);
			socket.once("connect", () => resolve(new Bridge(socket)));
			socket.once("error", reject);
		});
	}

	private write(message: unknown) {
		this.socket.write(`${JSON.stringify(message)}\n`);
	}

	hello(token: string): Promise<any> {
		return new Promise((resolve) => {
			this.waiting.set(0, resolve);
			this.write({ type: "hello", token });
		});
	}

	request(message: Record<string, unknown>, signal?: AbortSignal): Promise<any> {
		const id = this.next++;
		return new Promise((resolve) => {
			this.waiting.set(id, resolve);
			signal?.addEventListener("abort", () => this.write({ type: "cancel", id }), { once: true });
			this.write({ ...message, id });
		});
	}
}

function applyProviders(pi: ExtensionAPI, providers: any[]) {
	for (const p of providers ?? []) {
		if (!p?.name) continue;
		const { name, ...config } = p;
		try {
			pi.registerProvider(name, config);
		} catch (error) {
			console.error(`aether-bridge: provider ${name}: ${error}`);
		}
	}
}

export default async function (pi: ExtensionAPI) {
	const path = process.env.AETHER_BRIDGE;
	const token = process.env.AETHER_BRIDGE_TOKEN;
	delete process.env.AETHER_BRIDGE;
	delete process.env.AETHER_BRIDGE_TOKEN;
	const cwd = process.cwd();
	// Pi's own bash, minus secrets in the command environment.
	pi.registerTool(
		createBashToolDefinition(cwd, {
			spawnHook: (ctx) => ({ ...ctx, env: filtered(ctx.env as Record<string, string | undefined>) }),
		}),
	);
	if (!path || !token) return;
	let bridge: Bridge;
	let config: any;
	try {
		bridge = await Bridge.open(path);
		config = await bridge.hello(token);
	} catch (error) {
		console.error(`aether-bridge: cannot reach Aether: ${error}`);
		return;
	}
	if (config?.type !== "config") {
		console.error("aether-bridge: Aether refused the connection");
		return;
	}
	applyProviders(pi, config.providers);
	bridge.onPush = (message) => {
		if (message?.type === "providers") applyProviders(pi, message.providers);
	};
	const bridged = new Set<string>();
	for (const tool of config.tools ?? []) {
		bridged.add(tool.name);
		pi.registerTool({
			name: tool.name,
			label: tool.label ?? tool.name,
			description: tool.description ?? tool.name,
			promptSnippet: tool.promptSnippet,
			parameters: tool.parameters ?? { type: "object", properties: {} },
			async execute(toolCallId: string, params: any, signal?: AbortSignal) {
				const result = await bridge.request({ type: "call", callId: toolCallId, tool: tool.name, args: params }, signal);
				if (result?.type !== "result") throw new Error("Aether stopped before the tool finished");
				return {
					content: result.content ?? [],
					details: result.details ?? {},
					isError: result.isError === true,
				};
			},
		} as any);
	}
	pi.on("tool_call", async (event: any) => {
		if (!GATED.has(event.toolName) && !bridged.has(event.toolName)) return;
		const answer = await bridge.request({
			type: "gate",
			callId: event.toolCallId,
			tool: event.toolName,
			input: event.input,
		});
		if (answer?.type !== "gate") return { block: true, reason: "Aether is not reachable" };
		if (!answer.allow) return { block: true, reason: answer.reason ?? "blocked by Aether" };
	});
}
