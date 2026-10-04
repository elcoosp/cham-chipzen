# cham-chipzen developer recipes.
# `just wr`  → watch-and-rebuild loop (primary development command)
# `just check` / `just test` / `just lint` / `just fmt` → standard cargo aliases
# `just run` → start the bot using env vars (CHIPZEN_BASE_URL etc.)

wr:
    watchexec -w ./wr.sh --clear -r "./wr.sh"

check:
    cargo check --workspace --all-targets

test:
    cargo test --workspace

test-all:
    cargo test --workspace
    cargo test --test chameleon_wiring -- --ignored

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings

run:
    cargo run --release -- \
        --agent-dir {{env_var_or_default("CHAM_ARTIFACT_DIR", "artifacts/agent")}} \
        --loop

clean:
    cargo clean
