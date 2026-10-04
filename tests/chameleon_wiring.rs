//! Integration tests verifying the Chameleon agent is correctly wired into
//! the Chipzen bot.
//!
//! The pure tests run unconditionally and exercise the adapter surface (verb
//! translation, platform fallbacks, trivial policy legality). The Chameleon
//! smoke tests are `#[ignore]`d by default — they require a real artifact
//! bundle (layout produced by `cham-cli train-buckets + train-bp +
//! train-router`). Point them at one via `CHAM_ARTIFACT_DIR` (default
//! `artifacts/agent`) and run:
//!
//!     cargo test --test chameleon_wiring -- --ignored
//!
//! If loading fails, the error message from `ChamBrain::load` names the
//! missing/mismatched artifact — exactly what the wiring test is meant to
//! surface.

use cham_chipzen::agent::{trivial_platform_reply, ChamBrain};
use std::path::PathBuf;

fn artifact_dir() -> PathBuf {
    std::env::var("CHAM_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("artifacts/agent"))
}

#[test]
fn trivial_reply_is_always_legal() {
    let va: Vec<String> = vec!["fold".into(), "call".into(), "raise".into()];
    let (verb, _) = trivial_platform_reply(50, 100, &va);
    assert!(va.contains(&verb), "got verb {verb:?}, not in {va:?}");
}

#[test]
fn trivial_reply_handles_empty_valid_actions() {
    let (verb, _) = trivial_platform_reply(50, 100, &[]);
    assert_eq!(verb, "fold");
}

#[test]
#[ignore = "requires a Chameleon artifact bundle (CHAM_ARTIFACT_DIR or artifacts/agent)"]
fn cham_brain_loads_from_bundle() {
    let dir = artifact_dir();
    assert!(
        dir.is_dir(),
        "artifact bundle not found at {} — train one via cham-cli \
         (train-buckets / train-bp / train-router) or set CHAM_ARTIFACT_DIR",
        dir.display()
    );
    let brain = ChamBrain::load(&dir, "mixture", 100, 0xCE41_3E7)
        .unwrap_or_else(|e| panic!("ChamBrain::load failed: {e}"));
    drop(brain);
}

#[test]
#[ignore = "requires a Chameleon artifact bundle"]
fn cham_brain_postflop_degrades_to_trivial_policy() {
    let dir = artifact_dir();
    let mut brain = ChamBrain::load(&dir, "mixture", 100, 0xCE41_3E7).expect("load");
    let va: Vec<String> = vec!["fold".into(), "check".into(), "call".into()];
    // Postflop turn: brain cannot bind a board, so should fall back to the
    // trivial policy rather than hard-fold. With to_call == 0 the trivial
    // policy checks (not folds) — proving the degradation is policy-driven,
    // not a fixed fold.
    let (verb, _params) =
        brain.decide_turn(0, 150, "flop", &va, &serde_json::Value::Null);
    assert_eq!(verb, "check", "postflop trivial fallback should check when free");
}

#[test]
#[ignore = "requires a Chameleon artifact bundle"]
fn cham_brain_returns_legal_action_on_turn_request() {
    let dir = artifact_dir();
    let mut brain = ChamBrain::load(&dir, "mixture", 100, 0xCE41_3E7).expect("load");
    let va: Vec<String> = vec!["fold".into(), "call".into(), "raise".into()];
    let (verb, _params) =
        brain.decide_turn(100, 150, "preflop", &va, &serde_json::Value::Null);
    assert!(va.contains(&verb), "brain returned {verb:?}, not in {va:?}");
}

#[test]
#[ignore = "requires a Chameleon artifact bundle"]
fn cham_brain_advances_shadow_state_across_decisions() {
    let dir = artifact_dir();
    let mut brain = ChamBrain::load(&dir, "mixture", 100, 0xCE41_3E7).expect("load");

    let va: Vec<String> = vec!["fold".into(), "call".into(), "raise".into()];

    // Alternate decisions and villain observations. This exercises the full
    // adapter path: turn_request classification -> ChamBrain::decide_turn ->
    // shadow State::apply -> ChamBrain::observe_opponent_action ->
    // ChameleonAgent::on_public_action. Mis-wired Chameleon calls surface as
    // either a panic or an illegal-verb assertion.
    for i in 0..5 {
        let (verb, _params) =
            brain.decide_turn(100, 150, "preflop", &va, &serde_json::Value::Null);
        assert!(va.contains(&verb), "iter {i}: brain returned illegal verb {verb:?}");
        brain.observe_opponent_action("call", &serde_json::Value::Null);
    }
    brain.on_match_end();
}
