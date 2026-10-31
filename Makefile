CARGO_TARGET_DIR ?= .cache/target
export CARGO_TARGET_DIR

.PHONY: lint test

lint:
	cargo fmt --manifest-path crates/gpui/Cargo.toml --all -- --check
	cargo fmt --manifest-path crates/gpui_wgpu/Cargo.toml --all -- --check
	cargo clippy --locked --manifest-path crates/gpui/Cargo.toml --all-targets --all-features
	cargo clippy --locked --manifest-path crates/gpui_wgpu/Cargo.toml --all-targets --all-features

test:
	cargo test --locked --manifest-path crates/gpui/Cargo.toml --lib --no-default-features
	cargo test --locked --manifest-path crates/gpui_wgpu/Cargo.toml --lib
