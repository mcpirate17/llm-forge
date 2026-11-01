//! Provider configuration names and the startup events already supported by Forge.
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Provider {
    Claude,
    Codex,
    Qwen,
    Grok,
}

impl Provider {
    pub(crate) fn config_path(self) -> &'static str {
        match self {
            Self::Claude => ".claude/settings.json",
            Self::Codex => ".codex/hooks.json",
            Self::Qwen => ".qwen/settings.json",
            Self::Grok => ".grok/hooks/workspace.json",
        }
    }

    pub(crate) fn full_hooks(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Qwen => "qwen",
            Self::Grok => "grok",
        }
    }

    pub(crate) fn startup_event(self) -> &'static str {
        match self {
            // Grok consumes prompt-hook output; its SessionStart stdout is ignored.
            Self::Grok => "UserPromptSubmit",
            _ => "SessionStart",
        }
    }
}
