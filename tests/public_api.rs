//! Compile-time contract on the crate's public surface.
//!
//! Removing, renaming, or changing the *kind* of any re-export listed here
//! breaks this module at compile time — an intentional canary so downstream
//! consumers cannot be silently broken by a refactor. This is the same
//! discipline the CHAMELEON workspace uses for its `Agent` trait surface.

#![allow(dead_code, clippy::all)]

/// Every item in this function is a public re-export we commit to keeping.
/// If you remove one, remove it from `src/lib.rs` **and** from here, and
/// update the crate's CHANGELOG with a note.
fn _public_surface_contract() {
    // --- Re-exports from `proto` ---
    let _ = cham_chipzen::bot_token_subprotocols;
    let _ = cham_chipzen::parse_frame;
    let _ = cham_chipzen::BOT_TOKEN_SUBPROTOCOL;
    let _ = cham_chipzen::CLIENT_NAME;
    let _ = cham_chipzen::CLIENT_VERSION;
    let _ = cham_chipzen::PROTOCOL_VERSIONS;

    // --- Re-exports from `strategy` ---
    let _ = cham_chipzen::decide;
    let _ = cham_chipzen::rejection_fallback;

    // --- Re-exports from `client` ---
    let _ = cham_chipzen::handle_match_message;
    let _ = cham_chipzen::play_match;
    let _ = cham_chipzen::resolve_gateway_url;
    let _ = cham_chipzen::run_once;
    let _ = cham_chipzen::wait_for_matched;

    // --- Types ---
    let _ = std::mem::size_of::<cham_chipzen::Frame>();
    let _ = std::mem::size_of::<cham_chipzen::StateView>();
    let _ = std::mem::size_of::<cham_chipzen::Decision>();
    let _ = std::mem::size_of::<cham_chipzen::Error>();
    let _: Option<cham_chipzen::OutFrame<'_>> = None;

    // --- Module paths remain reachable (they are `pub mod`s) ---
    let _: Option<cham_chipzen::proto::StateView> = None;
    let _: Option<cham_chipzen::strategy::Decision> = None;
    let _: Option<cham_chipzen::error::Error> = None;
}

#[test]
fn public_surface_compiles() {
    // The assertion is the module above compiling; referencing every
    // re-export forces a compile error if any is removed.
}
