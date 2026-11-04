# Audience research: adopting local AI coding and tool assistants

**Accessed:** 2026-11-02. This is a small documentation and public-forum sample, not a representative user study. Product facts below come from vendor documentation. Forum reports are individual anecdotes and are not prevalence estimates. No personal data was collected; reports are attributed by thread/issue rather than user identity.

## What official product docs make a user install or decide

| Product/docs | Setup and ongoing requirements documented | Verification, privacy, and removal cues |
|---|---|---|
| **Ollama + coding-tool launch** ([launch](https://ollama.com/blog/launch), [FAQ](https://docs.ollama.com/faq), [CLI](https://docs.ollama.com/cli), [pricing](https://ollama.com/pricing)) | Install Ollama v0.15+, pull a chosen model, then `ollama launch <tool>`; for example `ollama launch claude`. Ollama says no environment variables or config file are needed for this integration. Its launch guide says coding tools work best with at least a 64K context and gives ~23 GB VRAM for `glm-4.7-flash` at that context length. Local and cloud model choices are both offered. | `ollama ps` shows loaded model and CPU/GPU placement; `ollama ls` lists downloaded models; `ollama rm <model>` deletes a model. Ollama says local prompts are not seen by its service, while cloud use is billed by model token rates/usage credits; models on the user's own hardware are unlimited in Ollama's usage policy. “Unlimited” does not include hardware, electricity, or storage cost. |
| **Cline local models** ([local models guide](https://github.com/cline/cline/blob/main/docs/running-models-locally/overview.mdx)) | Install a local runtime (Ollama, LM Studio, or Atomic Chat), start its server, choose the provider and model in Cline, and enable Compact Prompt. The guide's rough RAM bands are 16–32 GB for small/quantized models, 32–64 GB for mid-size coding models, and 64 GB+ for larger models/bigger contexts. It directs users to pull and run a model with Ollama before selecting it in Cline. | The guide asks users to confirm the local server endpoint and that the model is downloaded. Cline Desktop's local models page is described as able to recommend/download/load a model and configure Cline. The reviewed local-model guide did not document a unified full uninstall procedure. |
| **Continue** ([Ollama provider](https://docs.continue.dev/customize/model-providers/top-level/ollama), [offline guide](https://docs.continue.dev/guides/running-continue-without-internet), [models](https://docs.continue.dev/customize/models), [FAQ/reset](https://docs.continue.dev/faqs)) | Install IDE extension; install/run a local runtime/model; map a model name and provider in `config.yaml` (or config UI). Custom model names may need explicit `tool_use` / `image_input` capabilities. Continue's local-model page warns that local models' limited tool calling/reasoning can make Agent mode challenging. For air-gapped use, install the VS Code `.vsix`, disable anonymous telemetry, set a local default model, and restart VS Code. | For Ollama verification, docs suggest checking `http://localhost:11434` and running `ollama serve`; troubleshooting includes model/endpoint checks and local state reset. Continue states offline use is possible after local configuration, but telemetry must be disabled for the air-gapped path. A full reset removes `~/.continue` (or `%USERPROFILE%\.continue`) plus the extension. |
| **Aider + Ollama** ([Ollama guide](https://aider.chat/docs/llms/ollama.html)) | Install Aider using its documented Python installer, ensure Ollama API base is reachable, pull the model, run `ollama serve` with context length (guide example: 8K), then launch Aider in the repo with `aider --model ollama_chat/<model>`. Windows requires setting endpoint via `setx` and restarting the shell if it is non-default. | The guide's quick path tests by starting a repo session; it warns that Ollama's 2K default is small for Aider and may silently discard excess context. Users can set a larger context in model settings. This creates a concrete need to make context limits visible. |
| **Cursor (hosted-model comparison)** ([privacy/data governance](https://cursor.com/docs/enterprise/privacy-and-data-governance), [data-use overview](https://cursor.com/data-use)) | This is not a local-model setup. Cursor's docs say AI requests send prompts/code context to model providers; optional Cloud Agents require repo access over time and temporarily store encrypted repo copies while they run. | Privacy Mode prevents training and is paired with provider zero-retention arrangements in the normal case, but it does not mean inference happens on the user's machine. Some named models have different retention behavior. This distinction is useful when explaining “local,” “private,” and “offline” as separate choices. |
| **Claude Code (hosted-model comparison)** ([getting started](https://code.claude.com/docs/en/getting-started)) | Install CLI/Desktop, use an internet connection, and authenticate with an eligible paid Claude plan or Console/API account; docs list 4 GB+ RAM. | `claude --version` and `claude doctor` verify installation/configuration. The docs list uninstall commands for native installs and package managers. This is evidence that verification and uninstall instructions are normal onboarding content even for cloud tools. |

### Concrete onboarding implications grounded in those docs

- **Model choice is also hardware/context choice.** The tools expose model selector/provider settings, but context lengths and memory constraints can prevent an otherwise installed model from handling a codebase task. The Cline guide publishes broad RAM tiers; Ollama gives a concrete model/context VRAM figure; Aider calls out a small default context. These are product-specific guidance, not universal minimum specs.
- **Chat working is not the same as an agent working.** Continue explicitly cautions about local model tool-use/reasoning capability; the Cline issue sample below shows an individual model emitting a tool-call shape the client did not execute. A good first-run needs an observable tool test, not only a text response.
- **“Local,” “offline,” and “private” need separate labels.** Continue documents an offline procedure that disables telemetry. Ollama describes local prompt handling separately from cloud models. Cursor's Privacy Mode is a hosted inference/data-retention policy, not local inference. Do not treat these as equivalent privacy promises.
- **Local is not cost-free.** Ollama's policy says own-hardware usage is unlimited; user still supplies capable compute, storage for model downloads, power, and maintenance. Cloud alternatives attach token-rate/usage-credit costs. No hardware purchase price or lifetime-cost estimate was established here.
- **Verification and cleanup can be first-class product affordances.** Official examples include `ollama ps`/`ollama ls`, `claude doctor`, and Continue's reset path. They make it possible to check runtime/model state and remove model/config data without requiring a support search.

## Public user pain reports sampled

These are reports by individuals, not evidence of how common the problems are. They identify concrete failure modes worth testing in onboarding.

1. **Context too small for a project task.** A poster in [“Is Local Coding even worth setting up”](https://www.reddit.com/r/LocalLLaMA/comments/1qgwup8/is_local_coding_even_worth_setting_up/) says they were new to local LLMs, had a 16 GB VRAM GPU, and found Qwen2.5 Coder 7B's context insufficient for even one project file. The visible thread includes suggestions to decompose work into smaller steps. This lines up with vendor docs' context and hardware cautions; it remains one user's experience.
2. **Provider/endpoint mismatch and timeouts.** A poster in [“Using Ollama as a server”](https://www.reddit.com/r/ollama/comments/1v2ych3/using_ollama_as_a_server/) reports Cline sessions were slow, inaccurate, and timing out. In the same thread they later report that selecting OpenAI-compatible mode rather than the Ollama provider and increasing the timeout resolved their setup. This is a useful example of a configuration problem being mistaken initially for model/hardware performance.
3. **Connection failures across an editor/WSL boundary.** In [Cline Discussion #1979](https://github.com/cline/cline/discussions/1979), a reporter using Windows 11 + WSL2 described “API streaming failed” and “MCP hub not available” with Ollama; they report trying reinstalls, firewall checks, alternate models, and endpoint checks. The thread had no replies at the time inspected. It is an anecdote, not a verified product defect.
4. **Tool-call format incompatibility.** A reporter in [Cline issue #10843](https://github.com/cline/cline/issues/10843) describes Qwen2.5-Coder 32B via Ollama emitting JSON tool calls that Cline treated as plain text, repeatedly prompting the model rather than creating files. The issue was opened May 18, 2026. This distinguishes good task intent/text from correct tool execution.
5. **Context compaction can fail at a boundary.** A reporter in [Cline issue #13989](https://github.com/cline/cline/issues/13989) documented overflow and missing compaction events on native Ollama despite explicitly configured context windows and tool-heavy turns. Cline contributors later said a fix was merged and released by Cline CLI 3.0.71 in October 2026. The report therefore demonstrates a class of compatibility failure, not necessarily an unresolved current bug.

## Audience/problem hypotheses (for strategy review, not market facts)

- **Hypothesis: start with the concrete job, not the Rust/Python migration story.** A lay user is more likely to understand “inspect a repo, answer with cited files, make a bounded edit, and verify it locally” than an architecture migration. Validate this wording in user interviews; this evidence sample cannot establish conversion impact.
- **Hypothesis: offer a guided first-run with three checks:** (1) detect/choose local runtime or provider, (2) show model/context fit in plain language, (3) run one harmless tool action and report whether it executed. The docs and reports above show setup touches provider selection, endpoint, model size/context, and tool compatibility.
- **Hypothesis: expose a local/cloud/offline mode choice with a truthful data-flow summary.** State where inference happens, whether network is required, whether telemetry is separate, and whether a hosted fallback sends code out. Vendor docs show these are different states.
- **Hypothesis: provide a “works on this computer” path plus a small-model fallback.** Hardware mismatch and context pressure recur in the sampled official guidance/reports. Give users a graceful way to try a smaller task/model or a hosted endpoint without implying that quality is identical.
- **Hypothesis: make exit as legible as entry.** A single cleanup page should distinguish uninstalling the editor extension/runtime, deleting model weights, and deleting local config/index/recovery data. This reduces uncertainty for a non-expert evaluating a new tool.

## Sources and access date

All URLs below were accessed 2026-11-02. Vendor documentation is used only for each vendor's stated setup, product behavior, and requirements; forum claims are attributed reports, not independently verified facts.

### Official product documentation

- Ollama, `ollama launch` (Jan. 23, 2026): https://ollama.com/blog/launch
- Ollama FAQ (local/cloud data handling, context, GPU status): https://docs.ollama.com/faq
- Ollama CLI reference (list/stop/remove models): https://docs.ollama.com/cli
- Ollama pricing and usage: https://ollama.com/pricing
- Cline, local models guide: https://github.com/cline/cline/blob/main/docs/running-models-locally/overview.mdx
- Continue, Ollama provider: https://docs.continue.dev/customize/model-providers/top-level/ollama
- Continue, offline setup: https://docs.continue.dev/guides/running-continue-without-internet
- Continue, model capability guidance: https://docs.continue.dev/customize/models
- Continue, FAQ/reset: https://docs.continue.dev/faqs
- Aider, Ollama setup: https://aider.chat/docs/llms/ollama.html
- Cursor, Privacy and Data Governance: https://cursor.com/docs/enterprise/privacy-and-data-governance
- Cursor, Data Use & Privacy Overview (last updated Sept. 3, 2026): https://cursor.com/data-use
- Anthropic, Claude Code getting started: https://code.claude.com/docs/en/getting-started

### Public user reports

- Reddit r/LocalLLaMA, “Is Local Coding even worth setting up”: https://www.reddit.com/r/LocalLLaMA/comments/1qgwup8/is_local_coding_even_worth_setting_up/
- Reddit r/ollama, “Using Ollama as a server”: https://www.reddit.com/r/ollama/comments/1v2ych3/using_ollama_as_a_server/
- Cline GitHub Discussion #1979, WSL2/Ollama connection errors: https://github.com/cline/cline/discussions/1979
- Cline GitHub issue #10843, Qwen2.5-Coder/Ollama tool-call parsing: https://github.com/cline/cline/issues/10843
- Cline GitHub issue #13989, native Ollama context compaction; contributor later reports fix in CLI 3.0.71: https://github.com/cline/cline/issues/13989
