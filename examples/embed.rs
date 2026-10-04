//! Minimal example: use `cham-chipzen` as a library without any network.
//!
//! Parses a synthetic `turn_request`, runs the trivial reference policy, and
//! serializes a `turn_action` reply — showing the frame pipeline that
//! `client::handle_match_message` drives internally.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example embed
//! ```
//!
//! Expected output (one JSON line):
//!
//! ```json
//! {"type":"turn_action","match_id":"example-match","request_id":42,"action":"call","params":{}}
//! ```

use cham_chipzen::{decide, parse_frame, OutFrame};
use serde_json::json;

fn main() {
    // Simulate a `turn_request` from the platform.
    let inbound = r#"{
        "type": "turn_request",
        "request_id": 42,
        "state": {
            "to_call": 50,
            "pot": 150,
            "min_raise": 100,
            "max_raise": 2000,
            "street": "preflop"
        },
        "valid_actions": ["fold", "call", "raise"]
    }"#;

    let frame = parse_frame(inbound).expect("valid turn_request JSON");
    assert_eq!(frame.r#type, "turn_request");

    // Coerce the numeric fields (defensively) and run the trivial policy.
    let state = frame.state.clone().unwrap_or_default();
    let decision = decide(state.to_call(), state.pot(), &frame.valid_actions);

    // Echo the request_id verbatim — the protocol contract.
    let request_id = frame.request_id.clone().unwrap_or(json!(null));

    let out = OutFrame::TurnAction {
        match_id: "example-match",
        request_id: &request_id,
        action: &decision.action,
        params: &decision.params,
    };

    println!("{}", out.to_json().expect("serialize turn_action"));
}
