<div align="center">
  <img src="docs/logo.png" alt="cham-chipzen Logo" width="200"/>
  <p>
    <strong>A Chipzen External-API poker bot driven by the CHAMELEON HUNL agent.</strong><br/>
    A single-crate Rust bot that speaks the Chipzen External-API WebSocket protocol end-to-end — lobby matchmaking, per-match gateway handshake, Layer-2 turn loop — and delegates every decision to the CHAMELEON <a href="https://github.com/elcoosp/chameleon">heads-up no-limit blueprint agent</a>. A shadow engine state bridges the wire protocol to the agent's type-level contracts, with taint discipline that refuses to corrupt the tracker when the shadow cannot be trusted.
  </p>
  <p>
    <img src="https://img.shields.io/badge/Rust-1.98%20%7C%202024-000000?style=flat-square&logo=rust" alt="Rust"/>
    <img src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" alt="License MIT"/>
    <img src="https://img.shields.io/badge/unsafe-forbidden-success?style=flat-square" alt="Unsafe forbidden"/>
    <img src="https://img.shields.io/badge/Protocol-External--API-4B32C3?style=flat-square" alt="External API"/>
    <img src="https://img.shields.io/badge/Brain-CHAMELEON%20blueprint-8B0000?style=flat-square" alt="CHAMELEON brain"/>
    <img src="https://img.shields.io/badge/Tests-unit%20%2B%20e2e-228B22?style=flat-square" alt="Tests"/>
    <img src="https://img.shields.io/badge/Fallback-trivial%20policy-FF4500?style=flat-square" alt="Trivial fallback"/>
    <img src="https://img.shields.io/badge/CI-fmt%20%2B%20clippy%20%2B%20test-007ACC?style=flat-square" alt="CI"/>
  </p>
</div>

---

# cham-chipzen

> [!NOTE]
> `cham-chipzen` is the Rust port of the reference bot at
> [`chipzen-sdk/examples/external-api-bot`](https://github.com/chipzen-ai/chipzen-sdk/tree/main/examples/external-api-bot).
> It is the *driver* around the CHAMELEON agent — the agent's *playing
> strength* is a function of the artifact bundle you train, not of this
> repository. Without a bundle, the bot degrades to the trivial check/call/fold
> reference policy so the protocol path is always exercisable.

---

## Table of Contents

- [Why cham-chipzen](#why-cham-chipzen)
- [Features](#features)
- [Architecture](#architecture)
- [Getting Started](#getting-started)
- [Usage](#usage)
- [Configuration](#configuration)
- [Artifacts and Integrity](#artifacts-and-integrity)
- [Development](#development)
- [Project Status](#project-status)

---

## Why cham-chipzen

Every guarantee the bot makes about its own behaviour is checkable from the
`turn_action` frames it emits and the log lines it writes.

- **The protocol is the contract.** The bot speaks only what
  `docs/EXTERNAL-API-BOT-PROTOCOL.md` (in the SDK) documents: `authenticate`
  → server `hello` → client `hello` → `turn_request` / `turn_action` →
  `match_end`. Unknown frame types are ignored, not crashed on; unknown
  fields are tolerated, not schema-checked to death.
- **The CHAMELEON agent runs on a real `Observables` view.** Every decision
  goes through `ChameleonAgent::act` on a shadow engine state; the tracker
  consumes leak-disciplined `PublicHistory` only (CHAMELEON I9).
- **No silent state divergence.** When the shadow cannot be trusted — a
  rejected `turn_action`, a shadow/platform actor mismatch, or a panic on the
  blocking pool — the current hand is *tainted*: remaining turns use the
  trivial reference policy, and the tracker skips the hand. Silent
  corruption is treated as worse than graceful degradation.
- **The trivial fallback is always legal.** Every outbound verb is chosen
  from the platform's `valid_actions`; raise sizes are clamped into
  `[min_raise, max_raise]`. The bot never sends a verb the server did not
  offer.
- **Runs are deterministic.** The hero's hole cards and the decision RNG are
  derived from `(seed, hand_idx, decision_seq, pot)` — a rerun with the same
  seed is bit-identical up to the agent's own determinism.

If "will this bot do what I think it does when the network misbehaves?" is the
question you ask most, this is what the taint discipline is for.

---

## Features

### Protocol (`src/client.rs`, `src/proto.rs`)

- **Two WebSocket legs**, raw JSON frames. Lobby at
  `/ws/external/bot/{bot_id}`; per-match gateway at the path the platform
  returns in `matched.gateway_ws_url`.
- **Token in `Sec-WebSocket-Protocol`** (CZ issue 2932) — never on a query
  string, so it can never leak into a proxy access log.
- **Plain `ws://` refused off localhost.** Only `localhost` / `127.0.0.1`
  may use `ws://`; everything else must be `wss://`.
- **Forward-compatible frame parser.** Malformed JSON yields `None`; unknown
  `type` values are ignored. `action` verbs are extracted from
  `action: "..."`, `action.name`, `action.type`, and `action_name` shapes.

### Brain (`src/agent.rs`)

- **CHAMELEON-backed decisions on preflop spots** through a shadow engine
  state. Hero holes sampled from 1326 combos via per-hand child RNGs.
- **Raise-size clamping** against the platform's advertised
  `min_raise`/`max_raise`, applied to both the shadow state *and* the
  outbound `params.to` so the two never diverge.
- **Hand taint discipline** — rejection, actor mismatch, or panic marks the
  hand; remaining turns use the trivial policy; tracker updates skipped.
- **Postflop degradation** — when no board is bindable, the bot uses the
  trivial reference policy rather than hard-folding, which loses strictly
  less than a fixed fold.
- **Mid-match hand boundaries** (`round_start` / `hand_start` / `deal`) reset
  the shadow so the next hand starts fresh and the RNG `hand_idx` advances.

### Fallback policy (`src/strategy.rs`)

- **Pure reference policy** — check when free, call when cheap relative to
  the pot (`to_call <= pot/2`), fold otherwise. Zero side effects, fully
  unit-testable without a WebSocket.
- **Rejection retry** — on `action_rejected`, the retry uses the SAME
  `request_id` with a guaranteed-legal verb from the rejection's
  `valid_actions`, mirroring the SDK contract.

### CLI (`src/main.rs`)

- **`--loop`** cycles the lobby after each match, with **exponential
  backoff** (1s → 2s → 4s → … capped at 60s) and a reset on any healthy
  cycle.
- **`--require-agent`** refuses to run if the CHAMELEON bundle cannot be
  loaded — no silent trivial-policy fallback in production.
- **Fail-fast config validation** — routing whitelist, `depth_bb > 0`,
  non-empty token, `cz_extbot_` prefix warning.

---

## Architecture

| Component | Path | Role |
|-----------|------|------|
| Entry point | `src/main.rs` | CLI parse, config validation, loop-with-backoff, brain load |
| Client | `src/client.rs` | Lobby + match WebSocket legs, handshake, game loop |
| Wire types | `src/proto.rs` | Frame parsing + out-frame serialization, `StateView` |
| Brain | `src/agent.rs` | `ChamBrain`: shadow state, taint, decision adapter |
| Fallback policy | `src/strategy.rs` | Trivial check/call/fold reference policy |
| Errors | `src/error.rs` | `Error` enum (WS, connection, IO) |
| E2E tests | `tests/protocol_e2e.rs` | Mock lobby + match WS servers drive `run_once` |
| Wiring tests | `tests/chameleon_wiring.rs` | Brain loading, shadow advance, tracker discipline (`#[ignore]`d) |

### Data flow

```
    ┌──────────────────────────────────────────────────────────────────┐
    │                              main.rs                             │
    │  CLI parse · validation · --loop · reconnect backoff · brain     │
    └─────────────────────────┬────────────────────────────────────────┘
                              │ run_once(base, bot_id, token, brain)
                              ▼
    ┌──────────────────────────────────────────────────────────────────┐
    │                             client.rs                            │
    │  wait_for_matched ─► resolve_gateway_url ─► play_match           │
    │       │                                          │               │
    │       ▼                                          ▼               │
    │  lobby WS (token in authenticate)      match WS (subprotocol)    │
    │                                          │                       │
    │                              handle_match_message(frame)         │
    └──────────────────────────────┬───────────────────────────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              ▼                    ▼                    ▼
        turn_request        opponent_action      match_end /
              │             round_start etc.     action_rejected
              ▼                    │                    │
        decide_action              │                    │
              │                    │                    │
              ▼                    ▼                    ▼
    ┌──────────────────────────────────────────────────────────────────┐
    │                             agent.rs                             │
    │  ChamBrain: shadow State + ChameleonAgent + taint + hand_idx     │
    │  decide_turn · observe_opponent_action · note_new_hand · etc.    │
    └─────────────┬────────────────────────────────────┬───────────────┘
                  ▼                                    ▼
        cham_agent::ChameleonAgent             cham_core::State
        (tracker · router · experts)           (shadow HUNL engine)
```

The shadow-state model, the failure taxonomy, and the taint lifecycle are
documented in [`docs/architecture.md`](docs/architecture.md).

---

## Getting Started

### Prerequisites

- **Rust** 1.98 or newer (install via [rustup](https://rustup.rs/)). The
  crate uses the 2024 edition.
- **The CHAMELEON workspace** checked out as a sibling directory:
  ```
  parent/
    chameleon/           # https://github.com/elcoosp/chameleon
    cham-chipzen/        # this repo
  ```
  `Cargo.toml` uses path deps (`../chameleon/crates/cham-{core,agent,router}`).
  If you vendor elsewhere, either update those paths or set
  `CHIPZEN_CHAM_ROOT` in the build environment.
- **Chipzen credentials** — a bot UUID and a `cz_extbot_…` API token,
  obtainable from the Chipzen dashboard.

### From source

```bash
git clone https://github.com/elcoosp/cham-chipzen.git
cd cham-chipzen
cargo build --release
```

The binary lands at `./target/release/cham-chipzen`.

### First run

```bash
export CHIPZEN_BASE_URL="wss://staging.chipzen.ai"
export CHIPZEN_BOT_ID="<your-bot-uuid>"
export CHIPZEN_EXTBOT_TOKEN="cz_extbot_<...>"

# Without an artifact bundle → trivial reference policy + warning
./target/release/cham-chipzen --agent-dir artifacts/agent --loop
```

> [!TIP]
> The in-repo justfile routes common workflows (`just check`, `just test`,
> `just lint`, `just fmt`, `just run`) so you never measure a stale binary.
> Prefer `just` during development.

---

## Usage

```
cham-chipzen --base-url URL --bot-id UUID --token TOKEN [OPTIONS]
```

| Flag | Env | What it does |
|------|-----|--------------|
| `--base-url URL` | `CHIPZEN_BASE_URL` | Platform origin, e.g. `wss://staging.chipzen.ai`. |
| `--bot-id UUID` | `CHIPZEN_BOT_ID` | External-API bot's UUID. |
| `--token TOKEN` | `CHIPZEN_EXTBOT_TOKEN` | `cz_extbot_…` API token. |
| `--loop` | — | Reconnect the lobby after each match. |
| `--agent-dir PATH` | — | CHAMELEON artifact bundle directory. Default `artifacts/agent`. |
| `--routing MODE` | — | `mixture` (default), `argmax`, `robust-only`, `bayes`. |
| `--depth-bb N` | — | Stack depth in bb; must match the trained set. Default `100`. |
| `--seed HEX` | — | Deterministic seed. Default `0x0CE4_13E7`. |
| `-v`, `--verbose` | `RUST_LOG` | Debug logging. |
| `--require-agent` | — | Refuse to run without a loadable CHAMELEON bundle. |

Exit codes: `0` clean exit, `1` failure (match error without `--loop`),
`2` usage error.

```bash
# One match, verbose
cham-chipzen --base-url wss://staging.chipzen.ai \
             --bot-id "$CHIPZEN_BOT_ID" --token "$CHIPZEN_EXTBOT_TOKEN" \
             --agent-dir artifacts/agent --verbose

# Production loop with a required bundle
cham-chipzen --loop --require-agent --routing mixture --depth-bb 100

# Local dev against a plain ws:// server
cham-chipzen --base-url ws://localhost:8001 --loop
```

---

## Configuration

All configuration is via CLI flags or the environment variables above; there
is no config file to drift out of sync.

| Variable | Default | Purpose |
|----------|---------|---------|
| `CHIPZEN_BASE_URL` | — (required) | Platform origin. |
| `CHIPZEN_BOT_ID` | — (required) | External-API bot UUID. |
| `CHIPZEN_EXTBOT_TOKEN` | — (required) | `cz_extbot_…` token. |
| `CHAM_ARTIFACT_DIR` | `artifacts/agent` | Override the bundle directory for the `just run` recipe. |
| `RUST_LOG` | `info` | Log filter (or `-v` for `debug`). |
| `CHAM_SEARCH_SOLVER` | `Rnr` | Solver variant passed to `SearchCfg` (search is disabled in external play). |
| `CHAM_FALLBACK_MODE` | `renorm` | Passed to `AgentMode`; `substitute` restores legacy fallback semantics. |

> [!WARNING]
> The bot refuses plain `ws://` off localhost. If you point it at a staging
> environment, use `wss://` — the check runs before the WebSocket handshake.

---

## Artifacts and Integrity

The bot consumes a CHAMELEON **agent bundle** at `--agent-dir` (default
`artifacts/agent`). Layout:

```
artifacts/agent/
├── abstraction.toml        # validated at load
├── buckets/                # abstraction bucket tables
├── router.bin              # optional; uniform router otherwise
├── experts/
│   ├── 0/policy.bin
│   ├── 1/policy.bin
│   ├── 2/policy.bin
│   └── 3/policy.bin
├── robust/policy.bin       # required
└── bayes/policy.bin        # optional
```

Produced by the CHAMELEON CLI:

```bash
cham-cli train-buckets  --out artifacts/agent
cham-cli train-bp       --in  artifacts/agent
cham-cli train-router   --in  artifacts/agent
```

The loader (`cham_agent::loader::load_agent`) verifies each artifact against
its embedded blake3 checksum. A **mixed-depth bundle is a hard error** —
retrain at the depth you pass via `--depth-bb`.

If `router.bin` is missing or unparseable, the bot logs at **WARN** level
and falls back to a uniform router (all experts equally weighted). The
fallback is deliberately loud: silent routing degradation is worse than a
visible one.

---

## Development

```bash
# Build
cargo build --workspace

# Test (unit + adapter + e2e; skips the artifact-requiring tests)
cargo test --workspace

# Test the CHAMELEON wiring (requires an artifact bundle)
CHAM_ARTIFACT_DIR=artifacts/agent cargo test --test chameleon_wiring -- --ignored

# Lint and format
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Watch-and-rebuild loop (the primary dev command)
just wr
```

### Test layout

| Suite | Needs | Covers |
|-------|-------|--------|
| `src/**::tests` (unit) | nothing | Verb translation, clamping, RNG seeding, URL shaping, proto frames, CLI parsing |
| `tests/protocol_e2e.rs` | nothing | Full lobby + match flow against in-process mock WS servers; rejection retry |
| `tests/chameleon_wiring.rs` | artifact bundle (`#[ignore]`d) | Brain load, shadow advance, hand taint, tracker discipline |

### Continuous integration

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs `fmt --check`,
`clippy -D warnings`, `check --all-targets`, and `test --workspace` on Linux
and macOS. The workflow checks out `elcoosp/chameleon` as a sibling directory
so the path deps resolve — this requires the chameleon repository to be
accessible to the CI runner (public, or configured with a token for a
private checkout).

---

## Project Status

### Working end-to-end

- **Full protocol path** — lobby matchmaking, per-match gateway handshake
  with subprotocol token, Layer-2 turn loop, `match_end` with clean results.
- **CHAMELEON brain** on preflop spots: shadow engine state, real
  `Observables` view, tracker fed only `&PublicHistory`, decisions via
  `ChameleonAgent::act`.
- **Failure-path discipline** — taint on rejection / actor mismatch / panic;
  tracker skipped for tainted hands; backoff on repeated reconnect failures.
- **End-to-end protocol tests** — mock lobby + match servers drive `run_once`
  through the whole flow, including the rejection retry with the same
  `request_id`.
- **CI** — fmt, clippy `-D warnings`, check, and test on Linux + macOS.

### Tracked gaps

- **Postflop is a placeholder.** The External API does not currently expose
  a bindable board in the `turn_request` subset we consume, so the blueprint
  agent runs preflop only; postflop degrades to the trivial reference
  policy. `StateView::board_cards()` / `street_name()` already parse both
  the string-array and `{rank,suit}` board shapes, ready for a future server
  version.
- **The shadow state is best-effort.** The API does not guarantee delivery
  of `opponent_action` frames; a missed frame leads to an actor mismatch on
  the next `turn_request`, which taints the hand. Fully robust handling
  would require a request-response reconciliation step the protocol does
  not currently offer.
- **`ChameleonAgent` search is disabled.** The river search machinery in
  `cham-search` needs G4-audited cache artifacts to run in the agent; those
  are not shipped with the external-play bundle, so `SearchCfg.enabled` is
  hard-coded `false`.
- **The trivial fallback is not a strategy.** It is a smoke-test path. A
  production deployment must pass `--require-agent` so a missing or invalid
  bundle is a fatal error, not a silent downgrade.
- **No metrics / observability beyond `tracing`.** No Prometheus, no
  OpenTelemetry. Structured log lines are the only signal.

> [!WARNING]
> `cham-chipzen` is not a turnkey product. It is the driver around the
> CHAMELEON agent; the guarantees it makes (protocol conformance, taint
> discipline, deterministic seeding, leak-disciplined tracker) are real —
> the *playing strength* of any particular deployment is a function of the
> artifact bundle you train, not of this repository.

---

<p align="center">
  <em>Bug reports, protocol edge cases, and design discussions are welcome.</em>
</p>
