//! Agent presets ("modes") a new chat can start with: dsh's Standard and
//! Minimal declarations, with the picker copy and help texts of dsh's
//! `dsh-client-ui-agent-preset` (MIT). The UI localizes them by `id`; these
//! English texts are the fallback.
use dshx_agent::Preset;
use serde_json::{Value, json};

struct Texts {
    name: &'static str,
    description: &'static str,
    intro: &'static str,
    explanation: &'static [&'static str],
    usage: &'static [&'static str],
}

fn texts(preset: Preset) -> Texts {
    match preset {
        Preset::Standard => Texts {
            name: "Standard",
            description: "Work with code, files, and information. Suitable for most tasks, with search, editing, terminal commands, and other tools available as needed.",
            intro: "Choose Standard mode when starting a new task. Describe what you want to accomplish, point to the relevant files, and explain how to check the result.",
            explanation: &[
                "### How it works",
                "The agent calls tools directly to read and edit files, search, and run terminal commands. It includes Skills, planning, goals, subagents, and context compaction.",
                "### When to choose it",
                "Start here for everyday coding, file work, and research. Standard mode can also write scripts and process files in batches.",
            ],
            usage: &[
                "### Fix a bug",
                "> Find out why submitting the search form twice makes the results disappear. Fix it and run the relevant tests. Explain the cause and what changed.",
                "Expected output: a code change, the relevant test results, and an explanation of the cause.",
                "### Organize project notes",
                "> Read the Markdown notes in this project. Summarize the agreed decisions and open questions, with links to the source files.",
                "Expected output: a summary with references that you can check against the original notes.",
            ],
        },
        Preset::Minimal => Texts {
            name: "Minimal",
            description: "The agent works using only a terminal tool. Useful for testing and comparing its basic performance.",
            intro: "Choose Minimal mode for a new task. For a comparison, hold the model, permissions, input, and starting workspace state constant across runs.",
            explanation: &[
                "### What is included",
                "One persistent shell tool and a fixed system prompt. The built-in preset does not load Skills, planning, context compaction, or the standard runtime context.",
                "### When to choose it",
                "Use it as a baseline for experiments and comparisons. It can still read files and execute scripts through shell commands, but offers fewer built-in ways to manage a long task. Fewer tools does not necessarily make it easier for a beginner.",
            ],
            usage: &[
                "### Compare performance on a small bug fix",
                "> Run the tests for this project, find the cause of the failure, and make the smallest fix. Run the relevant tests again and report the result.",
                "Run the same task separately in Standard and Minimal modes from the same starting state. Compare task completion, tool calls, and the resulting changes. Minimal mode performs the work through terminal commands.",
            ],
        },
    }
}

/// `GET /api/info` `presets`: `[{id, name, description, help: {intro,
/// explanation, usage}}]` (help in Markdown), in roster order.
pub fn catalog() -> Value {
    Value::Array(
        Preset::ALL
            .into_iter()
            .map(|preset| {
                let c = texts(preset);
                json!({
                    "id": preset.id(),
                    "name": c.name,
                    "description": c.description,
                    "help": {
                        "intro": c.intro,
                        "explanation": c.explanation.join("\n\n"),
                        "usage": c.usage.join("\n\n"),
                    },
                })
            })
            .collect(),
    )
}

/// A preset id from a request or the config.
pub fn parse(id: &str) -> anyhow::Result<Preset> {
    Preset::parse(id).ok_or_else(|| {
        let known: Vec<&str> = Preset::ALL.iter().map(|p| p.id()).collect();
        anyhow::anyhow!("unknown agent preset {id:?} (known: {})", known.join(", "))
    })
}

/// The configured preset for new chats; an unknown id falls back to Standard.
pub fn default_of(config: &dshx_core::config::Config) -> Preset {
    config
        .default_preset
        .as_deref()
        .and_then(Preset::parse)
        .unwrap_or_default()
}
