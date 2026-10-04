//! Pure decision policy — port of `strategy.py` from the reference bot.
//!
//! Deliberately protocol-free and side-effect-free so it is unit-testable
//! without any WebSocket plumbing (see `tests/strategy.rs`).

use serde_json::{Map, Value};

/// A chosen action: name + params (`params` is always present, empty for
/// non-raise actions, so callers can splat it into a `turn_action` frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub action: String,
    pub params: Value,
}

impl Decision {
    fn simple(action: &str) -> Decision {
        Decision {
            action: action.to_string(),
            params: Value::Object(Map::new()),
        }
    }
}

/// Trivial reference policy: check when free, call when cheap relative to the
/// pot (`to_call <= pot / 2`), otherwise fold; belt-and-suspenders fallbacks
/// mirror the Python exactly.
///
/// * `to_call` / `pot` — read from the Layer-2 `turn_request.state`; absent or
///   unparseable values are treated as 0 so a sparse state never panics.
/// * `valid_actions` — the legal-action strings for this turn.
pub fn decide(to_call: i64, pot: i64, valid_actions: &[String]) -> Decision {
    let has = |a: &str| valid_actions.iter().any(|v| v == a);

    // Free to check -> check.
    if to_call <= 0 && has("check") {
        return Decision::simple("check");
    }

    // Cheap to call -> call. "Cheap" = at most half the current pot. Guard the
    // pot == 0 edge so a 0-pot, nonzero-to_call spot doesn't auto-call.
    if has("call") && to_call > 0 && to_call <= pot / 2 {
        return Decision::simple("call");
    }

    // Otherwise fold if we can.
    if has("fold") {
        return Decision::simple("fold");
    }

    // Belt-and-suspenders: the server's auto-action policy guarantees one of
    // check/fold is always legal, but prefer the cheapest legal action anyway.
    if has("check") {
        return Decision::simple("check");
    }
    if has("call") {
        return Decision::simple("call");
    }
    // Last resort: echo the first legal action with empty params.
    match valid_actions.first() {
        Some(a) => Decision::simple(a),
        None => Decision::simple("fold"),
    }
}

/// Guaranteed-legal fallback on `action_rejected` (port of the retry branch in
/// `client.handle_match_message`): check if offered, else fold. The server's
/// auto-action guarantee means either is accepted with empty params.
pub fn rejection_fallback(valid_actions: &[String]) -> Decision {
    if valid_actions.is_empty() {
        // Field absent -> fall back per the server's auto-action guarantee.
        return Decision::simple("check");
    }
    if valid_actions.iter().any(|v| v == "check") {
        Decision::simple("check")
    } else {
        Decision::simple("fold")
    }
}
