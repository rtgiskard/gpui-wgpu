CARGO_TARGET_DIR ?= .cache/target
export CARGO_TARGET_DIR

.PHONY: lint test

lint:
	cargo fmt --all -- --check
	cargo clippy --locked --workspace --all-targets --all-features

test:
	cargo test --locked -p gpui --lib --no-default-features
	cargo test --locked -p gpui_wgpu --lib
	cargo test --locked -p gpui_linux --lib
	cargo test --locked -p gpui_platform --lib
