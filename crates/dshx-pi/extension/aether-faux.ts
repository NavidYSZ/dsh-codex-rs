// Aether's scripted test model for the Pi engine: provider `pi-faux`, model
// `faux-1`. It never touches the network. Loaded only when DSHX_PI_TEST=1
// (or by the dshx-pi live test). The last transcript message picks the reply:
//
//   bash: <command>   one `bash` call, then a short answer
//   read: <path>      one `read` call
//   write: <path>     one `write` call with "hello\n"
//   edit: <path>      one `edit` call replacing "hello" with "bye"
//   todo              one `todo_write` call (Aether's bridge tool)
//   slow <text>       waits 1.5 s (abortable), then answers
//   fail              a provider error
//   flaky             a rate-limit error first, then an answer (Pi retries)
//   anything else     thinking plus "Echo: <text>"
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
	fauxAssistantMessage,
	fauxProvider,
	fauxText,
	fauxThinking,
	fauxToolCall,
} from "@earendil-works/pi-ai";

function textOf(message: any): string {
	const content = message?.content;
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.filter((b: any) => b?.type === "text")
		.map((b: any) => b.text)
		.join("");
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
	return new Promise((resolve) => {
		const timer = setTimeout(resolve, ms);
		signal?.addEventListener("abort", () => {
			clearTimeout(timer);
			resolve();
		});
	});
}

export default function (pi: ExtensionAPI) {
	const faux = fauxProvider({
		provider: "pi-faux",
		models: [{ id: "faux-1", name: "Faux 1", reasoning: true, contextWindow: 64000, maxTokens: 4096 }],
	});
	let flaky = 0;
	const reply = async (context: any, options: any) => {
		const messages = context?.messages ?? [];
		const last = messages[messages.length - 1];
		if (last?.role === "toolResult") {
			const first = textOf(last).split("\n")[0].slice(0, 80);
			return fauxAssistantMessage([fauxText(`Done with ${last.toolName}: ${first}`)]);
		}
		const text = textOf(last).trim();
		const arg = (prefix: string) => text.slice(prefix.length).trim();
		if (text.startsWith("bash:")) {
			return fauxAssistantMessage([fauxThinking("Running it."), fauxToolCall("bash", { command: arg("bash:") })], {
				stopReason: "toolUse",
			});
		}
		if (text.startsWith("read:")) {
			return fauxAssistantMessage([fauxToolCall("read", { path: arg("read:") })], { stopReason: "toolUse" });
		}
		if (text.startsWith("write:")) {
			return fauxAssistantMessage([fauxToolCall("write", { path: arg("write:"), content: "hello\n" })], {
				stopReason: "toolUse",
			});
		}
		if (text.startsWith("edit:")) {
			return fauxAssistantMessage(
				[fauxToolCall("edit", { path: arg("edit:"), edits: [{ oldText: "hello", newText: "bye" }] })],
				{ stopReason: "toolUse" },
			);
		}
		if (text === "todo") {
			return fauxAssistantMessage(
				[
					fauxToolCall("todo_write", {
						todos: [
							{ content: "Plan", status: "completed" },
							{ content: "Build", status: "in_progress" },
						],
					}),
				],
				{ stopReason: "toolUse" },
			);
		}
		if (text.startsWith("slow")) {
			await sleep(1500, options?.signal);
			return fauxAssistantMessage([fauxText(`Slow answer: ${arg("slow")}`)]);
		}
		if (text === "fail") {
			return fauxAssistantMessage([], { stopReason: "error", errorMessage: "faux provider failure" });
		}
		if (text === "flaky") {
			flaky += 1;
			if (flaky % 2 === 1) {
				return fauxAssistantMessage([], { stopReason: "error", errorMessage: "429 rate limit exceeded" });
			}
			return fauxAssistantMessage([fauxText("Recovered after a retry.")]);
		}
		return fauxAssistantMessage([fauxThinking("Thinking about it."), fauxText(`Echo: ${text}`)]);
	};
	faux.setResponses(Array.from({ length: 10000 }, () => reply));
	pi.registerProvider(faux.provider as any);
}
