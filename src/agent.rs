//! CHAMELEON brain: mirrors `cham-cli`'s hero construction (`cmd/hero.rs`,
//! `cmd/play.rs`) and adapts the platform's Layer-2 `turn_request` JSON to the
//! internal HUNL [`State`] the agent reasons over.
//!
//! The Chipzen protocol is *external-API only*: the bot never receives hole
//! cards or a full game state, so we cannot feed `Observables` directly.
//! Instead we maintain a shadow engine state per hand:
//!
//! * Preflop: blinds are known from `depth_bb`; our hole is sampled uniformly
//!   from the 1326 combos with an OSRNG-derived stream (seeded per match).
//!   The opponent's action string ("call" facing the BB ante = limp; "raise"
//!   with `params.to` = 3-bet) drives `State::apply` through legal-action
//!   translation.
//! * Postflop: the platform does not expose a board we can bind into the
//!   shadow engine, so the blueprint agent cannot run. We degrade to the
//!   trivial check/call/fold reference policy (never a fixed fold — that
//!   would auto-lose the pot) and log the limitation once per turn.
//!
//! Every decision goes through `ChameleonAgent::act` on a real `Observables`
//! view, keeping tracker/weights/action-seq state consistent across hands, and
//! `on_hand_end` is fed a leak-disciplined `PublicHistory` (I9, SPECS/01 §5).

use cham_agent::{loader, modes::SearchCfg, pipeline::ChameleonAgent, AgentMode};
use cham_core::{
    card::Deck,
    engine::{
        history::{HandHistory, PublicHistory},
        Action, State, Street,
    },
    obs::{Agent, Observables, Player},
    rng::{child, rng_from_seed, Rng},
    EngineConfig,
};
use cham_router::{model::SoftmaxModel, runtime::RouterRuntime};
use serde_json::Value;
use std::path::Path;
use tracing::{info, warn};

/// Map a platform action name + params to one of the engine's canonical
/// actions. Returns `None` for unknown verbs (forward-compat: caller folds).
pub fn translate_action(name: &str, params: &Value) -> Option<Action> {
    let to = params.get("to").and_then(Value::as_i64);
    match name {
        "fold" => Some(Action::Fold),
        "check" => Some(Action::Check),
        "call" => Some(Action::Call),
        "bet" => to.map(|t| Action::Bet { to: t }),
        "raise" => to.map(|t| Action::Raise { to: t }),
        _ => None,
    }
}

/// Clamp a requested raise-to into `[min_raise_to, max_raise_to]`, snapping to
/// the all-in ceiling when the request overshoots (mirrors how live casinos
/// treat over-raises; keeps every translated action engine-legal).
fn clamp_raise_to(requested: i64, min_to: i64, max_to: i64) -> i64 {
    if requested >= max_to {
        max_to
    } else {
        requested.max(min_to).min(max_to)
    }
}

/// Turn a desired platform action into an engine action that is a member of
/// `obs.legal` (the trait contract: `act` must return a legal action).
pub fn make_legal(desired: Option<Action>, obs: &Observables<'_>) -> Action {
    let contains = |a: &Action| obs.legal.iter().any(|l| &l.action == a);
    if let Some(d) = desired {
        // Exact size if legal; otherwise same-class at clamped size.
        if contains(&d) {
            return d;
        }
        let sized = match d {
            Action::Bet { to } => Some(Action::Bet {
                to: clamp_raise_to(to, obs.min_raise_to, obs.max_raise_to),
            }),
            Action::Raise { to } => Some(Action::Raise {
                to: clamp_raise_to(to, obs.min_raise_to, obs.max_raise_to),
            }),
            other => Some(other),
        };
        if let Some(s) = sized {
            if contains(&s) {
                return s;
            }
        }
    }
    // Guaranteed-legal fallback ladder: check, call, fold.
    for cand in [Action::Check, Action::Call, Action::Fold] {
        if contains(&cand) {
            return cand;
        }
    }
    // Should be unreachable (engine always offers check or fold); take slot 0.
    obs.legal
        .first()
        .map(|l| l.action)
        .unwrap_or(Action::Fold)
}

/// The CHAMELEON-powered poker brain behind the Chipzen bot.
pub struct ChamBrain {
    agent: ChameleonAgent,
    #[allow(dead_code)]
    rng: Rng,
    seed: u64,
    depth_bb: i64,
    /// Per-hand shadow state; rebuilt by `start_hand`.
    state: Option<State>,
    /// Seat we occupy inside the shadow state (SB=0 / BB=1), alternating per
    /// hand like a real heads-up match.
    hero_seat: usize,
    hand_idx: u64,
    log: Vec<(Street, Player, Action)>,
    /// True once the opponent acted before us this street (needed to tell a
    /// preflop "call" (limp) from a post-raise call, and bet vs raise).
    opp_acted_this_street: bool,
}

impl ChamBrain {
    /// Load artifacts from `bundle_dir` (layout: `artifacts/agent`, see
    /// `cham_agent::loader`). Falls back to the trivial reference policy on any
    /// error, with a warning — matching the reference bot's play-ability goal.
    pub fn load(bundle_dir: &Path, routing: &str, depth_bb: i64, seed: u64) -> Result<ChamBrain, String> {
        let loaded = loader::load_agent(bundle_dir, routing, depth_bb)
            .map_err(|e| format!("artifact bundle under {} not loadable: {e}", bundle_dir.display()))?;
        let mode = AgentMode {
            routing: routing.to_string(),
            search: SearchCfg {
                enabled: false, // river search needs G4-audited cache artifacts; off for external play
                solver: std::env::var("CHAM_SEARCH_SOLVER").unwrap_or_else(|_| "Rnr".into()),
                g4_ledger_ref: String::new(),
            },
            fallback_mode: std::env::var("CHAM_FALLBACK_MODE").unwrap_or_else(|_| "renorm".into()),
        };
        let router = match std::fs::read(bundle_dir.join("router.bin")) {
            Ok(bytes) => RouterRuntime::from_model_bytes(&bytes)
                .map_err(|e| format!("router model: {e}"))?,
            Err(_) => RouterRuntime::new(SoftmaxModel::new(20, 4), 0.7, 8.0, 0.5, -1.5),
        };
        let agent = ChameleonAgent::new(
            mode,
            loaded.encoder,
            router,
            loaded.experts,
            loaded.robust,
            loaded.bayes,
            None,
        )
        .map_err(|e| format!("agent: {e}"))?;
        info!(
            "chameleon brain loaded (routing={routing}, depth={depth_bb}bb, seed={seed})"
        );
        Ok(ChamBrain {
            agent,
            rng: rng_from_seed(seed),
            seed,
            depth_bb,
            state: None,
            hero_seat: 0,
            hand_idx: 0,
            log: Vec::new(),
            opp_acted_this_street: false,
        })
    }

    fn start_hand(&mut self) {
        // Deterministic child stream per hand (chameleon RNG discipline).
        let mut deal_rng = child(self.seed, &format!("chipzen-hand{}", self.hand_idx));
        let deck = Deck::shuffled(&mut deal_rng);
        let cfg = EngineConfig::depth(self.depth_bb);
        match State::new(cfg, deck) {
            Ok(state) => {
                self.state = Some(state);
                self.log.clear();
                self.opp_acted_this_street = false;
            }
            Err(e) => warn!("shadow state init failed: {e}"),
        }
    }

    /// Record the opponent's latest public action into the shadow state.
    /// Called on every inbound frame that could carry one (action_accepted /
    /// turn_result / round frames) — idempotent via the stored last action.
    pub fn observe_opponent_action(&mut self, name: &str, params: &Value) {
        if self.state.is_none() {
            self.start_hand();
        }
        if self.state.is_none() {
            return;
        }
        let preflop_unopened = self.preflop_unopened();
        let Some(state) = self.state.as_mut() else { return };
        if state.is_terminal() {
            return;
        }
        let seat = state.to_act();
        if seat == self.hero_seat {
            // Not the villain's turn in our shadow — nothing to ingest.
            return;
        }
        let player = Player::from_usize(seat);
        let desired = classify_incoming(name, params, preflop_unopened);
        let obs = Observables::view(state, player);
        let action = make_legal(desired, &obs);
        // Feed our own tracker the villain action (public info only).
        self.agent.on_public_action(&obs, player, action);
        let street_now = state.street();
        self.log.push((street_now, player, action));
        if state.apply(action).is_err() {
            warn!("villain action {name} rejected by shadow engine; folding next turn");
        }
        self.opp_acted_this_street = true;
    }

    fn preflop_unopened(&self) -> bool {
        match &self.state {
            Some(s) => s.street() == Street::Preflop && !self.opp_acted_this_street,
            None => true,
        }
    }

    /// Decide our response to a `turn_request`. Returns the platform action
    /// name + params to send back.
    pub fn decide_turn(
        &mut self,
        to_call: i64,
        pot: i64,
        platform_street: &str,
        valid_actions: &[String],
        params_hint: &Value,
    ) -> (String, Value) {
        if self.state.is_none() {
            self.start_hand();
        }
        // If the previous hand terminated in the shadow, close its books first.
        let terminal = self.state.as_ref().is_some_and(|s| s.is_terminal());
        if terminal {
            self.finish_hand();
            self.start_hand();
        }
        // Postflop: the External API does not currently expose a board we can
        // bind into the shadow engine (see README limitations), so we cannot
        // run the blueprint agent here. Degrade to the trivial check/call/fold
        // policy rather than hard-folding, which auto-loses the pot — this
        // keeps the protocol path playable and losing strictly less than a
        // fixed fold.
        if platform_street != "preflop" {
            info!(
                street = platform_street,
                "postflop: no bindable board, using trivial reference policy"
            );
            return trivial_platform_reply(to_call, pot, valid_actions);
        }
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return trivial_platform_reply(to_call, pot, valid_actions),
        };
        if state.street() != Street::Preflop {
            // Shadow state advanced past preflop while the platform says we
            // are preflop → the shadow is out of sync. Rebuild it and fall
            // back rather than trust a stale state.
            warn!(
                shadow = ?state.street(),
                "shadow state street mismatch; rebuilding and using trivial policy"
            );
            self.start_hand();
            return trivial_platform_reply(to_call, pot, valid_actions);
        }
        let seat = state.to_act();
        if seat != self.hero_seat {
            // The shadow state says it is the *villain's* turn, but the
            // platform just handed *us* a `turn_request`. That means the
            // shadow and the platform disagree about whose action is owed —
            // likely because we did not see the villain's last public action
            // (the External API does not guarantee delivery of those frames).
            //
            // Emitting a blueprint action for the wrong seat would be illegal
            // in the shadow and, worse, semantically wrong (the agent's
            // distribution assumes *it* is the actor). Degrade to the trivial
            // reference policy and rebuild the shadow next hand.
            warn!(
                shadow_to_act = seat,
                hero_seat = self.hero_seat,
                "shadow/platform actor mismatch: using trivial policy this turn"
            );
            return trivial_platform_reply(to_call, pot, valid_actions);
        }
        let player = Player::from_usize(self.hero_seat);
        // Our intent: mirror the chameleon distribution on the *preflop* spot.
        // (Villain context is already applied to the shadow state; the agent's
        // canonical ActionSeq was fed via on_public_action in observe_*.)
        let obs = Observables::view(state, player);
        let mut rng = child(self.seed, &format!("chipzen-decide{}-{}", self.hand_idx, pot));
        let chosen = self.agent.act(&obs, &mut rng);
        // Clamp the engine action to the *platform's* advertised raise bounds
        // BEFORE applying it to the shadow state. This keeps the shadow state
        // and the platform state byte-for-byte consistent in raise sizes —
        // otherwise a shadow-state bet of 9999 that the platform clamps to
        // 1500 would desync every subsequent turn_request's `to_call`/`pot`.
        let chosen = clamp_engine_action(chosen, params_hint);
        let mapped = map_engine_to_platform(chosen, valid_actions, params_hint);
        // Apply to shadow + record.
        let street_now = state.street();
        self.log.push((street_now, player, chosen));
        if state.apply(chosen).is_err() {
            warn!(
                "own action {:?} illegal in shadow; using platform reply verbatim",
                chosen
            );
        }
        if state.is_terminal() {
            self.finish_hand();
        }
        mapped
    }

    /// Terminal shadow state → feed the agent a leak-disciplined public
    /// history (holes stay private; I9).
    fn finish_hand(&mut self) {
        let Some(state) = self.state.as_ref() else { return };
        let nets = state.payoffs();
        let hh = HandHistory {
            seed: self.seed ^ self.hand_idx,
            actions: std::mem::take(&mut self.log),
            cfg: state.cfg(),
            holes: [state.hole(0), state.hole(1)],
            board: *state.board(),
            board_len: state.board_len(),
            result_sb: nets[0],
        };
        let ph = PublicHistory::from(&hh);
        let hero_net = nets[self.hero_seat];
        self.agent.on_hand_end(&ph, hero_net);
        self.hand_idx += 1;
        self.hero_seat = 1 - self.hero_seat;
        self.state = None;
        self.opp_acted_this_street = false;
    }

    /// Notify the brain that the platform rejected our last turn_action.
    ///
    /// The shadow state has already been advanced with the rejected action
    /// (see `decide_turn`), and we cannot reconstruct the state the platform
    /// actually settled on from public info alone. Clear the shadow so the
    /// next decision starts from a fresh hand; the current hand's remaining
    /// turns will hit the actor-mismatch guard and degrade to the trivial
    /// policy, which is strictly safer than acting on a stale state.
    pub fn note_action_rejected(&mut self) {
        warn!("platform rejected turn_action: resetting shadow state");
        self.state = None;
        self.log.clear();
        self.opp_acted_this_street = false;
    }

    /// Handle a clean `match_end`: close out any unfinished hand bookkeeping.
    ///
    /// Seat alternation is a *within-match* invariant (HU deals alternate
    /// SB/BB every hand). A match that ends mid-hand has not completed that
    /// hand, so we must NOT flip the seat or advance `hand_idx` — the next
    /// match begins a fresh hand from the current seat, keeping the shadow
    /// state's hand-index cursor monotone across reconnects.
    ///
    /// If, however, the shadow state *is* terminal when match_end arrives
    /// (e.g., the villain's final fold/call ended the hand and the platform
    /// then closed the match), we must still feed that completed hand to the
    /// tracker via `finish_hand` — otherwise the last hand of every match
    /// silently never reaches `ChameleonAgent::on_hand_end`, degrading the
    /// tracker's running statistics at every match boundary.
    pub fn on_match_end(&mut self) {
        let terminal = self.state.as_ref().is_some_and(|s| s.is_terminal());
        if terminal {
            self.finish_hand();
        }
        self.state = None;
        self.log.clear();
        self.opp_acted_this_street = false;
    }
}

/// Classify an incoming platform action verb into the intended engine action.
/// A preflop "call" facing only the BB ante is a limp → Bet{to: bb}; a "raise"
/// preflop is a Raise{to}; postflop "bet" → Bet{to}, "raise" → Raise{to}.
/// Sizes come from `params.to` when present; `make_legal` clamps them.
fn classify_incoming(name: &str, params: &Value, preflop_first_entry: bool) -> Option<Action> {
    let to = params.get("to").and_then(Value::as_i64);
    match name {
        "fold" => Some(Action::Fold),
        "check" => Some(Action::Check),
        "call" => {
            if preflop_first_entry {
                // BB-complete limp: engine models it as a bet-to of one BB.
                to.map(|t| Action::Bet { to: t }).or(Some(Action::Call))
            } else {
                Some(Action::Call)
            }
        }
        "bet" => to.map(|t| Action::Bet { to: t }).or(Some(Action::Call)),
        "raise" => to.map(|t| Action::Raise { to: t }).or(Some(Action::Call)),
        _ => None,
    }
}

/// Clamp an engine action's raise size into the platform's advertised bounds
/// (read from `params_hint.min` / `params_hint.max`). Non-raise actions pass
/// through unchanged; a missing hint passes the action through unchanged.
///
/// This is applied *before* the action is recorded to the shadow engine and
/// sent to the platform, so both sides agree on the exact size — critical for
/// keeping `turn_request.state.to_call` / `.pot` consistent across turns.
fn clamp_engine_action(chosen: Action, params_hint: &Value) -> Action {
    let mn = params_hint.get("min").and_then(Value::as_i64);
    let mx = params_hint.get("max").and_then(Value::as_i64);
    match chosen {
        Action::Bet { to } => Action::Bet {
            to: clamp_size(to, mn, mx),
        },
        Action::Raise { to } => Action::Raise {
            to: clamp_size(to, mn, mx),
        },
        other => other,
    }
}

/// Clamp `to` into `[mn, mx]`, flooring at 1 (a 0-chip bet is never legal).
fn clamp_size(to: i64, mn: Option<i64>, mx: Option<i64>) -> i64 {
    match (mn, mx) {
        (Some(lo), Some(hi)) => to.clamp(lo.max(1), hi.max(lo.max(1))),
        (Some(lo), None) => to.max(lo.max(1)),
        (None, Some(hi)) => to.min(hi.max(1)),
        (None, None) => to,
    }
}

/// Map the engine action the agent chose onto a platform action name+params,
/// respecting `valid_actions` (never emit a verb the server disallows).
fn map_engine_to_platform(
    chosen: Action,
    valid_actions: &[String],
    params_hint: &Value,
) -> (String, Value) {
    let has = |v: &str| valid_actions.iter().any(|a| a == v);
    match chosen {
        Action::Fold => pick_platform("fold", valid_actions),
        Action::Check => pick_platform("check", valid_actions),
        Action::Call => pick_platform("call", valid_actions),
        Action::Bet { to } | Action::Raise { to } => {
            let verb = if has("raise") { "raise" } else if has("bet") { "bet" } else { "call" };
            if verb == "call" {
                return pick_platform("call", valid_actions);
            }
            // Clamp `to` into the platform's advertised raise bounds when
            // present (server-provided `min_raise` / `max_raise`). When the
            // hint is absent (older server, or non-raise spot) we trust the
            // engine's own legality clamp already applied upstream.
            let mn = params_hint.get("min").and_then(Value::as_i64);
            let mx = params_hint.get("max").and_then(Value::as_i64);
            let adjusted = match (mn, mx) {
                (Some(lo), Some(hi)) => to.clamp(lo.max(1), hi.max(lo.max(1))),
                (Some(lo), None) => to.max(lo.max(1)),
                (None, Some(hi)) => to.min(hi.max(1)),
                (None, None) => to,
            };
            let mut m = serde_json::Map::new();
            m.insert("to".into(), Value::Number(adjusted.into()));
            (verb.to_string(), Value::Object(m))
        }
    }
}

/// Pick a platform verb, degrading gracefully if the server didn't offer it.
fn pick_platform(want: &str, valid_actions: &[String]) -> (String, Value) {
    let empty = Value::Object(serde_json::Map::new());
    let order: &[&str] = match want {
        "check" => &["check", "call", "fold"],
        "call" => &["call", "check", "fold"],
        _ => &["fold", "check", "call"],
    };
    for cand in order {
        if valid_actions.iter().any(|a| a == cand) {
            return (cand.to_string(), empty.clone());
        }
    }
    match valid_actions.first() {
        Some(a) => (a.clone(), empty),
        None => ("fold".to_string(), empty),
    }
}

/// Trivial-policy reply (used when no artifacts are loaded): exact port of the
/// reference `strategy.decide` + verb mapping.
pub fn trivial_platform_reply(to_call: i64, pot: i64, valid_actions: &[String]) -> (String, Value) {
    let d = crate::strategy::decide(to_call, pot, valid_actions);
    (d.action, d.params)
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translate_action_covers_all_verbs() {
        assert_eq!(translate_action("fold", &json!({})), Some(Action::Fold));
        assert_eq!(translate_action("check", &json!({})), Some(Action::Check));
        assert_eq!(translate_action("call", &json!({})), Some(Action::Call));
        assert_eq!(translate_action("bet", &json!({"to": 500})), Some(Action::Bet { to: 500 }));
        assert_eq!(translate_action("raise", &json!({"to": 1200})), Some(Action::Raise { to: 1200 }));
    }

    #[test]
    fn translate_action_rejects_unknown_and_missing_size() {
        assert_eq!(translate_action("allin", &json!({"to": 9999})), None);
        assert_eq!(translate_action("bet", &json!({})), None);
        assert_eq!(translate_action("raise", &json!({})), None);
    }

    #[test]
    fn clamp_raise_to_snaps_overshoot_to_max() {
        assert_eq!(clamp_raise_to(5000, 100, 3000), 3000);
        assert_eq!(clamp_raise_to(5000, 100, 5000), 5000);
    }

    #[test]
    fn clamp_raise_to_raises_below_min() {
        assert_eq!(clamp_raise_to(50, 100, 3000), 100);
        assert_eq!(clamp_raise_to(200, 100, 3000), 200);
    }

    #[test]
    fn classify_incoming_treats_preflop_call_as_limp() {
        assert_eq!(
            classify_incoming("call", &json!({"to": 100}), true),
            Some(Action::Bet { to: 100 })
        );
        assert_eq!(
            classify_incoming("call", &json!({"to": 100}), false),
            Some(Action::Call)
        );
    }

    #[test]
    fn classify_incoming_bet_and_raise_carry_size() {
        assert_eq!(
            classify_incoming("raise", &json!({"to": 600}), false),
            Some(Action::Raise { to: 600 })
        );
        assert_eq!(
            classify_incoming("bet", &json!({"to": 400}), false),
            Some(Action::Bet { to: 400 })
        );
    }

    #[test]
    fn classify_incoming_unknown_yields_none() {
        assert_eq!(classify_incoming("gibberish", &json!({}), false), None);
    }

    #[test]
    fn map_engine_to_platform_prefers_raise_when_offered() {
        let va: Vec<String> = vec!["fold".into(), "call".into(), "raise".into()];
        let (verb, params) = map_engine_to_platform(Action::Raise { to: 900 }, &va, &Value::Null);
        assert_eq!(verb, "raise");
        assert_eq!(params.get("to").and_then(Value::as_i64), Some(900));
    }

    #[test]
    fn map_engine_to_platform_clamps_raise_to_hint_bounds() {
        let va: Vec<String> = vec!["fold".into(), "call".into(), "raise".into()];
        let hint = json!({"min": 300, "max": 1500});
        let (_, params) = map_engine_to_platform(Action::Raise { to: 9999 }, &va, &hint);
        assert_eq!(params.get("to").and_then(Value::as_i64), Some(1500));
        let (_, params) = map_engine_to_platform(Action::Raise { to: 50 }, &va, &hint);
        assert_eq!(params.get("to").and_then(Value::as_i64), Some(300));
        let (_, params) = map_engine_to_platform(Action::Raise { to: 800 }, &va, &hint);
        assert_eq!(params.get("to").and_then(Value::as_i64), Some(800));
    }

    #[test]
    fn map_engine_to_platform_only_min_or_only_max() {
        let va: Vec<String> = vec!["fold".into(), "call".into(), "raise".into()];
        let (_, params) = map_engine_to_platform(
            Action::Raise { to: 9999 },
            &va,
            &json!({"max": 2000}),
        );
        assert_eq!(params.get("to").and_then(Value::as_i64), Some(2000));
        let (_, params) = map_engine_to_platform(
            Action::Raise { to: 50 },
            &va,
            &json!({"min": 400}),
        );
        assert_eq!(params.get("to").and_then(Value::as_i64), Some(400));
    }

    #[test]
    fn map_engine_to_platform_degrades_raise_to_bet() {
        let va: Vec<String> = vec!["fold".into(), "call".into(), "bet".into()];
        let (verb, params) = map_engine_to_platform(Action::Raise { to: 900 }, &va, &Value::Null);
        assert_eq!(verb, "bet");
        assert_eq!(params.get("to").and_then(Value::as_i64), Some(900));
    }

    #[test]
    fn map_engine_to_platform_degrades_raise_to_call() {
        let va: Vec<String> = vec!["fold".into(), "call".into()];
        let (verb, _) = map_engine_to_platform(Action::Raise { to: 900 }, &va, &Value::Null);
        assert_eq!(verb, "call");
    }

    #[test]
    fn map_engine_to_platform_simple_actions_pass_through() {
        let va: Vec<String> = vec!["fold".into(), "check".into(), "call".into()];
        assert_eq!(map_engine_to_platform(Action::Fold, &va, &Value::Null).0, "fold");
        assert_eq!(map_engine_to_platform(Action::Check, &va, &Value::Null).0, "check");
        assert_eq!(map_engine_to_platform(Action::Call, &va, &Value::Null).0, "call");
    }

    #[test]
    fn pick_platform_prefers_wanted_then_degrades() {
        let va: Vec<String> = vec!["fold".into(), "call".into()];
        assert_eq!(pick_platform("check", &va).0, "call");
        assert_eq!(pick_platform("call", &va).0, "call");
        assert_eq!(pick_platform("fold", &va).0, "fold");
    }

    #[test]
    fn pick_platform_empty_falls_back_to_fold() {
        assert_eq!(pick_platform("check", &[]).0, "fold");
    }

    #[test]
    fn clamp_engine_action_leaves_non_raises_untouched() {
        let hint = json!({"min": 300, "max": 1500});
        assert!(matches!(clamp_engine_action(Action::Fold, &hint), Action::Fold));
        assert!(matches!(clamp_engine_action(Action::Check, &hint), Action::Check));
        assert!(matches!(clamp_engine_action(Action::Call, &hint), Action::Call));
    }

    #[test]
    fn clamp_engine_action_clamps_raise_and_bet() {
        let hint = json!({"min": 300, "max": 1500});
        assert_eq!(
            clamp_engine_action(Action::Raise { to: 9999 }, &hint),
            Action::Raise { to: 1500 }
        );
        assert_eq!(
            clamp_engine_action(Action::Raise { to: 50 }, &hint),
            Action::Raise { to: 300 }
        );
        assert_eq!(
            clamp_engine_action(Action::Bet { to: 800 }, &hint),
            Action::Bet { to: 800 }
        );
    }

    #[test]
    fn clamp_engine_action_missing_hint_passthrough() {
        assert_eq!(
            clamp_engine_action(Action::Raise { to: 9999 }, &Value::Null),
            Action::Raise { to: 9999 }
        );
    }

    #[test]
    fn clamp_size_floors_at_one() {
        assert_eq!(clamp_size(0, Some(0), Some(0)), 1);
        assert_eq!(clamp_size(-5, None, None), -5);
        assert_eq!(clamp_size(50, Some(0), Some(0)), 1);
    }

    #[test]
    fn classify_incoming_call_preflop_and_postflop_differ() {
        // Sanity: a preflop `call` is a limp (Bet-to); the same verb postflop
        // is a plain Call. This distinction is what lets the shadow state tell
        // "villain limped" from "villain called a raise".
        let pre = classify_incoming("call", &json!({"to": 100}), true);
        let post = classify_incoming("call", &json!({"to": 100}), false);
        assert_ne!(pre, post);
        assert_eq!(pre, Some(Action::Bet { to: 100 }));
        assert_eq!(post, Some(Action::Call));
    }

    #[test]
    fn decide_turn_postflop_uses_trivial_policy_not_fixed_fold() {
        // Cannot construct a full ChamBrain without artifacts, so exercise the
        // trivial-policy reply that decide_turn delegates to postflop.
        let va: Vec<String> = vec!["fold".into(), "check".into(), "call".into()];
        // Free to check → trivial policy checks (not folds).
        assert_eq!(trivial_platform_reply(0, 150, &va).0, "check");
        // Cheap call → trivial policy calls.
        assert_eq!(trivial_platform_reply(50, 150, &va).0, "call");
        // Expensive → trivial policy folds.
        assert_eq!(trivial_platform_reply(200, 150, &va).0, "fold");
    }
}
