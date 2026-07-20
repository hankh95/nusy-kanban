# nusy-llm-backend

A small, pluggable LLM client for NuSy. One `LlmClient` trait, a handful of
backends, and **one env var** to switch between them — the "easy LLM plug-in"
the V19 launch ruling (2026-06-15) requires (EX-5134).

## Plug in your LLM with one env var

Select a backend at runtime with `LLM_BACKEND`:

| `LLM_BACKEND`          | Backend                | Endpoint (default)        | Key |
|------------------------|------------------------|---------------------------|-----|
| `mock`                 | deterministic mock     | — (CPU / offline)         | none |
| `vllm` / `local-vllm`  | OpenAI-compatible      | `http://localhost:8000/v1`  | optional (`OPENAI_API_KEY`) |
| `openai`               | OpenAI-compatible      | `https://api.openai.com/v1` | **required** (`OPENAI_API_KEY`) |
| `ollama`               | OpenAI-compatible      | `http://localhost:11434/v1` | none |
| `claude` / `anthropic` | Anthropic Messages API | Anthropic cloud           | **required** (`ANTHROPIC_API_KEY`) |
| *(unset)*              | `claude`               | Anthropic cloud           | **required** |

```rust
use nusy_llm_backend::{Backend, LlmClient, LlmParams};

// Reads LLM_BACKEND (+ OPENAI_BASE_URL / OPENAI_API_KEY / OPENAI_MODEL).
let llm = Backend::from_env("gpt-4o-mini")?;
let answer = llm.complete("Summarize JNC8 in one line.", &LlmParams::default()).await?;
```

```sh
# Local Ollama (no key):
LLM_BACKEND=ollama OPENAI_MODEL=llama3.1 ./your-bin

# OpenAI:
LLM_BACKEND=openai OPENAI_API_KEY=sk-... OPENAI_MODEL=gpt-4o-mini ./your-bin

# Any other OpenAI-compatible host (Together, Groq, LM Studio) — one adapter,
# many providers: keep LLM_BACKEND=openai and point OPENAI_BASE_URL at it:
LLM_BACKEND=openai OPENAI_BASE_URL=https://api.groq.com/openai/v1 \
  OPENAI_API_KEY=gsk-... OPENAI_MODEL=llama-3.3-70b-versatile ./your-bin
```

A single `OpenAiBackend` speaks `/v1/chat/completions`, so **one adapter covers
many providers** (OpenAI, local vLLM, Together, Groq, LM Studio, Ollama's OpenAI
mode) — selected via `OpenAiProvider` and an optional `OPENAI_BASE_URL` override.

Selection fails **loudly**: an unknown `LLM_BACKEND` value is a config error (not
a silent default), and a hosted provider missing its required key is a config
error too — never a silent unauthenticated call.

## Honest LLM requirements (don't overstate the neural remainder)

The LLM here is a **proposer behind a provable gate**, not an oracle. In the
NuSy reasoner-router, an LLM answer is `Provability::Heuristic` — *structurally
incapable* of being laundered into `Proven` (that invariant is type-level; see
`nusy-llm-reasoner`). What the LLM is *for* is the flagged-neural and
abstention-explanation paths, and even there its quality is bounded: H-4916
calibrates the abstention path's "name the missing datum" accuracy at a target
of ≈0.85 across the NCCN set — useful, not infallible. **Plugging in a bigger
model improves the neural remainder; it never makes a heuristic answer proven.**
Pick a model sized to your task; the symbolic gate, not the model, is what
carries the proof.

## PHI / egress safety

This crate is **FOSS and PHI-agnostic** — the adapter itself knows nothing about
patients. When a prompt may carry PHI and the endpoint is off-box (e.g.
`LLM_BACKEND=openai`), wrap the client in the egress gate (`EgressGate`,
EX-4985): it enforces an endpoint allowlist, TLS, and a de-identification
precondition, and **fails closed**. Do not send PHI to a cloud endpoint without
it.

## Testing

`cargo test -p nusy-llm-backend` — unit tests plus a `wiremock` round-trip that
drives the selectable router against a fake OpenAI-compatible endpoint (no
network). Provider-profile resolution is a pure function (`build_openai_compatible`),
unit-tested with no env-var races.
