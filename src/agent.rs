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
//! * Postflop: the platform exposes no board in the turn_request subset we can
//!   rely on, so we fold (the honest default for an unknown-spot blueprint
//!   agent) — recorded as a limitation in the README.
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
    last_action_name: String,
    last_params: Value,
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
            last_action_name: String::new(),
            last_params: Value::Null,
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
                self.last_action_name.clear();
                self.last_params = Value::Null;
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
        self.last_action_name = name.to_string();
        self.last_params = params.clone();
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
        let state = match self.state.as_mut() {
            Some(s) => s,
            None => return trivial_platform_reply(to_call, pot, valid_actions),
        };
        if state.street() != Street::Preflop {
            // No board visibility on the external API → honest fold postflop.
            warn!("postflop turn without board data: folding (see README limitations)");
            return pick_platform("fold", valid_actions);
        }
        let seat = state.to_act();
        if seat != self.hero_seat {
            // Defensive: shouldn't happen with well-formed turn_requests —
            // villain still owes an action in our shadow.
            warn!("turn_request while villain to act in shadow state");
        }
        let player = Player::from_usize(self.hero_seat);
        // Our intent: mirror the chameleon distribution on the *preflop* spot.
        // (Villain context is already applied to the shadow state; the agent's
        // canonical ActionSeq was fed via on_public_action in observe_*.)
        let obs = Observables::view(state, player);
        let mut rng = child(self.seed, &format!("chipzen-decide{}-{}", self.hand_idx, pot));
        let chosen = self.agent.act(&obs, &mut rng);
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

    /// Handle a clean `match_end`: close out any unfinished hand bookkeeping.
    pub fn on_match_end(&mut self) {
        if let Some(state) = self.state.take() {
            if !state.is_terminal() {
                // Match cut short mid-hand: no reliable payoff; skip tracker
                // update rather than poison it with a fabricated history.
                self.hand_idx += 1;
                self.hero_seat = 1 - self.hero_seat;
            }
        }
        self.log.clear();
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

/// Map the engine action the agent chose onto a platform action name+params,
/// respecting `valid_actions` (never emit a verb the server disallows).
fn map_engine_to_platform(
    chosen: Action,
    valid_actions: &[String],
    _params_hint: &Value,
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
            let mut m = serde_json::Map::new();
            m.insert("to".into(), Value::Number(to.into()));
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

