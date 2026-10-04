# Changelog

All notable changes to this project are documented here.
The format is loosely based on [Keep a Changelog](https://keepachangelog.com/)
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **CHAMELEON brain wiring** — the bot now runs the CHAMELEON blueprint agent
  on preflop spots, maintaining a shadow engine state per hand and feeding
  `ChameleonAgent::on_public_action` / `on_hand_end` with leak-disciplined
  history.
- **`--require-agent`** — refuse to start if the artifact bundle fails to
  load, rather than silently degrading to the trivial policy.
- **Raise bounds parsing** — `StateView` now reads `min_raise` / `max_raise`
  from the platform's `turn_request.state` and clamps every outbound raise
  into that window, keeping the shadow state and platform state consistent.
- **Board / street parsing** — `StateView::board_cards()` and
  `StateView::street_name()` tolerate both `["Ah","Kd"]` and
  `[{"rank":"A","suit":"h"}]` shapes, ready for a future server that exposes
  the board.
- **Hand taint discipline** — a rejected `turn_action` or a shadow/platform
  actor mismatch marks the current hand as tainted: subsequent turns degrade
  to the trivial policy, and the tracker skips the hand so its statistics are
  never poisoned.
- **RNG stream uniqueness** — the decision RNG is seeded with
  `(seed, hand_idx, decision_seq, pot)` so two same-pot turns in one hand
  cannot draw identical actions.
- **End-to-end protocol tests** (`tests/protocol_e2e.rs`) against in-process
  mock WebSocket servers — the whole lobby → matched → match handshake →
  `turn_request` → `turn_action` → `match_end` flow, with no real network.
- **GitHub Actions CI** — fmt, clippy `-D warnings`, `check --all-targets`,
  and `test --workspace` on Linux and macOS.

### Changed

- **Postflop policy** — no longer hard-folds. Postflop turns degrade to the
  trivial check/call/fold reference policy, which loses strictly less than a
  fixed fold when no board is bindable.
- **`on_match_end`** — closes a terminal shadow hand (feeding the tracker)
  but does **not** flip the seat or advance `hand_idx` when the match ended
  mid-hand, so per-match state stays consistent across reconnects.
- **Unclean socket close** — the shadow state is reset via `on_match_end` so
  a mid-hand network drop cannot leak stale state into the next match.
- **Router loading** — the fallback to a uniform router is now logged at
  `WARN` level with the offending path, so silent routing degradation is
  impossible.
- **Dependency bumps** — `tokio-tungstenite 0.24 → 0.29`, `thiserror 1 → 2`,
  edition `2021 → 2024`, `rust-version 1.85 → 1.98`. `clap` gains the `env`
  feature so CLI flags can be set from environment variables.

### Fixed

- **`normalise_base`** — keeps only an explicit port from the input URL;
  known-default ports (80 for `http://`, 443 for `https://`) no longer leak
  into the normalized form.
- **`map_engine_to_platform`** — clamps requested raise sizes against the
  platform's advertised `min_raise` / `max_raise`, so the emitted
  `params.to` is always server-legal.
- **`extract_action_name`** — tolerates `action: "..."`, `action.name`,
  `action.type`, and `action_name` shapes for forward compatibility with
  future platform versions.
- **Compiler/clippy hygiene** — no warnings under
  `clippy --all-targets -- -D warnings`.

## [0.1.0] — initial port

- First Rust port of `chipzen-sdk/examples/external-api-bot`.
- Lobby + match WebSocket client, Layer-1 handshake, game loop.
- Trivial check/call/fold reference policy as a fallback.
