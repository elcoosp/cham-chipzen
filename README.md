# cham-chipzen

A **Chipzen External-API poker bot** driven by the
[CHAMELEON](../chameleon) heads-up no-limit (HUNL) blueprint agent. Rust port
of the reference bot at
[`chipzen-sdk/examples/external-api-bot`](https://github.com/chipzen-ai/chipzen-sdk/tree/main/examples/external-api-bot).

## What it does

1. Opens the **lobby WebSocket** at `/ws/external/bot/{bot_id}`, sends the
   `authenticate` frame with the `cz_extbot_…` token, and waits for a
   `matched` notification.
2. Resolves `gateway_ws_url` against the base origin and opens the **match
   WebSocket**. The token travels in the `Sec-WebSocket-Protocol` header
   (CZ issue 2932 — never on the query string).
3. Runs the Layer-1 handshake (`authenticate` → server `hello` → client
   `hello`), then the game loop until `match_end`.
4. On every `turn_request`, asks the CHAMELEON `ChamBrain` for an action. If
   artifacts are missing, it degrades to the trivial check/call/fold
   reference policy so the protocol path is always exercisable.

## Requirements

- Rust **1.98** or newer (edition **2024**).
- The CHAMELEON workspace checked out as a sibling directory:
  ```
  parent/
    chameleon/           # https://github.com/elcoosp/chameleon
    cham-chipzen/        # this repo
  ```
  The `Cargo.toml` uses path deps (`../chameleon/crates/cham-{core,agent,router}`).
  If you vendor this crate elsewhere, update those paths or provide a
  `CHIPZEN_CHAM_ROOT` override in your build environment.

## Configuration

All configuration is via CLI flags or environment variables (clap `env`
feature):

| Flag                    | Env var                    | Meaning                              |
| ----------------------- | -------------------------- | ------------------------------------ |
| `--base-url`            | `CHIPZEN_BASE_URL`         | Platform origin (e.g. `wss://staging.chipzen.ai`, or `ws://localhost:8001` for local dev). |
| `--bot-id`              | `CHIPZEN_BOT_ID`           | The External-API bot's UUID.         |
| `--token`               | `CHIPZEN_EXTBOT_TOKEN`     | The `cz_extbot_…` API token.         |
| `--loop`                | —                          | Reconnect the lobby after each match.|
| `--agent-dir`           | —                          | CHAMELEON artifact bundle dir. Default `artifacts/agent`. |
| `--routing`             | —                          | `mixture` (default), `argmax`, `robust-only`, `bayes`. |
| `--depth-bb`            | —                          | Stack depth in bb; must match the trained set. Default `100`. |
| `--seed`                | —                          | Deterministic seed for hole sampling.|
| `-v`, `--verbose`       | `RUST_LOG`                 | Debug logging.                       |

## Running

```sh
export CHIPZEN_BASE_URL="wss://staging.chipzen.ai"
export CHIPZEN_BOT_ID="<your-bot-uuid>"
export CHIPZEN_EXTBOT_TOKEN="cz_extbot_<...>"

cargo run --release -- --agent-dir artifacts/agent --loop
```

Without `--agent-dir` pointing at a valid bundle, the bot logs a warning and
plays the trivial reference policy — useful for protocol smoke-testing.

## CHAMELEON artifact bundle

The brain expects a bundle produced by `cham-cli`:

```sh
cham-cli train-buckets  --out artifacts/agent
cham-cli train-bp       --in  artifacts/agent
cham-cli train-router   --in  artifacts/agent
```

The loader (`cham_agent::loader::load_agent`) reads the bundle directory and
verifies each artifact against a blake3 checksum. A mixed-depth bundle is a
hard error — retrain at the depth you pass via `--depth-bb`.

## Postflop limitation

The External API, at the time of writing, does **not** expose the board in the
`turn_request.state` payload we can bind into a shadow engine. The preflop
brain therefore runs only on preflop spots. Postflop, the bot **degrades to
the trivial reference policy** (check when free, call when cheap relative to
the pot, fold otherwise) rather than hard-folding — this keeps the protocol
path playable and loses strictly less than a fixed fold.

If a future server version adds a `state.board` array of card strings (or
`{rank,suit}` objects) and a `state.street` field, `StateView::board_cards()`
and `StateView::street_name()` already parse both forms, so wiring them into
the shadow engine is a small, contained change.

## How the CHAMELEON wiring works

The bot maintains a **shadow engine state** per hand, because the External API
only sends action/state deltas, never a full game state:

- **Preflop**: blinds come from `depth_bb`; the hero's hole cards are sampled
  uniformly from the 1326 combos using a per-hand child RNG seeded from
  `--seed` and the hand index. The villain's action string (`"call"`
  facing the BB ante = limp; `"raise"` with `params.to` = 3-bet) is
  translated into an engine `Action` and applied.
- **Every decision** goes through `ChameleonAgent::act` on a real
  `Observables` view, keeping tracker/weights/action-seq state consistent
  across hands.
- **Hand end** feeds the agent a leak-disciplined `PublicHistory`
  (holes stay private; I9 in `SPECS/01 §5`).

The wiring is covered by `tests/chameleon_wiring.rs`:

- Pure adapter tests run unconditionally.
- `#[ignore]`d smoke tests require a real bundle. Run them with:
  ```sh
  CHAM_ARTIFACT_DIR=artifacts/agent \
    cargo test --test chameleon_wiring -- --ignored
  ```

## Tests

```sh
cargo test --workspace                     # unit + adapter tests
cargo test --test chameleon_wiring -- --ignored  # needs artifacts
```

## License

MIT.
