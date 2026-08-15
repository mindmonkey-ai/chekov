# Thin by design (bootstrap prompt §6): every target <=3 lines, zero logic
# duplicated from the CLI. hermes/claude integration is `chekov integrate`.

.PHONY: setup update install test lint

setup:
	cargo build --release
	./target/release/chekov setup

update:
	chekov update --all

install:
	cargo install --path .
	chekov completions zsh > shell/_chekov
	grep -qF 'source ~/personal_dev/chekov/shell/chekov.zsh' ~/.zshrc || echo 'source ~/personal_dev/chekov/shell/chekov.zsh' >> ~/.zshrc

test:
	cargo test

lint:
	cargo fmt --check && cargo clippy --all-targets -- -D warnings
