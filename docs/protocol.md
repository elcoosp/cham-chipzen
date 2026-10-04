# Chipzen External-API protocol (as consumed by cham-chipzen)

This document is the maintainer-facing summary of the subset of the Chipzen
External-API WebSocket protocol that `cham-chipzen` consumes and emits. It is
derived from `docs/EXTERNAL-API-BOT-PROTOCOL.md` in the Chipzen SDK and from
the reference `client.py`.

## Legs

The bot opens two WebSocket legs per match:

| Leg   | URL                                          | Token location                      |
| ----- | -------------------------------------------- | ----------------------------------- |
| lobby | `{base}/ws/external/bot/{bot_id}`            | `authenticate` frame body           |
| match | `{base}{gateway_ws_url}` (path from `matched`) | `Sec-WebSocket-Protocol` header (CZ #2932) |

Plain `ws://` is refused off localhost, by both the client and (independently)
the test harness. `wss://` is mandatory in production.

## Frame envelope

Every frame is a JSON object with a string `type` discriminator. Unknown
fields are tolerated; unknown `type`s are ignored (forward compatibility).

```json
{ "type": "turn_request", "...": "..." }
```

## Lobby handshake

```
client → { "type": "authenticate", "token": "cz_extbot_…" }
server → { "type": "hello", "endpoint": "lobby" }
server → { "type": "ping" }                         # heartbeat
client → { "type": "pong" }
server → { "type": "matched",
           "match_id": "…",
           "participant_id": "…",
           "gateway_ws_url": "/ws/external/match/{mid}/{pid}",
           "rated": false }
```

If the socket closes before `matched`, `wait_for_matched` returns
`Error::Protocol` — the `--loop` mode treats this as fatal for the current
attempt but retries with backoff (see `src/main.rs`).

## Match handshake (Layer 1)

```
client → { "type": "authenticate", "token": "", "match_id": "…" }
server → { "type": "hello", "selected_version": "1.0", "game_type": "hu-nl" }
client → { "type": "hello",
           "match_id": "…",
           "supported_versions": ["1.0"],
           "client_name": "chipzen-extapi-rust",
           "client_version": "0.1.0" }
```

The first `authenticate` carries an empty token by protocol — the gateway’s
internal JWT is authoritative — but it MUST be the first frame or the
handshake stalls.

## Match loop (Layer 2)

```
server → { "type": "turn_request",
           "request_id": 42,
           "state": { "to_call": 100,
                      "pot": 150,
                      "min_raise": 200,
                      "max_raise": 2000,
                      "street": "preflop" },
           "valid_actions": ["fold", "call", "raise"] }
client → { "type": "turn_action",
           "match_id": "…",
           "request_id": 42,             # echoed verbatim
           "action": "raise",
           "params": { "to": 500 } }     # {} for non-raise verbs

server → { "type": "action_accepted" }        # echo of our action
server → { "type": "opponent_action",
           "action": "raise",
           "params": { "to": 600 } }
server → { "type": "action_rejected",
           "request_id": 42,
           "reason": "illegal_action",
           "valid_actions": ["fold", "check"] }
client → { "type": "turn_action",
           "match_id": "…",
           "request_id": 42,             # same request_id on retry
           "action": "check",
           "params": {} }

server → { "type": "round_start" }            # new hand within the match
server → { "type": "match_end",
           "reason": "normal",
           "results": [1, -1] }
```

## Guarantees the bot relies on

- `request_id` in `turn_action` MUST echo the corresponding `turn_request`.
- `valid_actions` on `turn_request` and `action_rejected` lists the verbs the
  server will accept for the current reply. Anything outside that set is a
  bug — `map_engine_to_platform` degrades rather than emit an illegal verb.
- `action_rejected` MUST be answered with a retry bearing the same
  `request_id`; the retry's verb MUST come from the rejection's
  `valid_actions` (or `check` when the field is absent — the server's
  auto-action guarantee).
- The server does not guarantee delivery of `opponent_action`. A missed frame
  surfaces as a `turn_request` while the shadow thinks the villain is to act —
  this taints the hand (see `docs/architecture.md`).

## What the bot does *not* consume

- Board cards on `turn_request` (only `state.board` if the server starts
  sending it — `StateView::board_cards()` already parses the two plausible
  shapes).
- Session tokens, reconnect flows, `session_control` — ignored.

## Testing

- `tests/protocol_e2e.rs` exercises the full lobby → matched → match →
  `turn_request` → `turn_action` → `match_end` flow against mock servers on
  ephemeral localhost ports. It also covers the `action_rejected` retry path.
- `src/proto.rs` unit tests cover each frame shape independently.
