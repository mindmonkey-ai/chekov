# Thin by design (bootstrap prompt §6): every target <=3 lines, zero logic
# duplicated from the CLI. hermes/claude integration is `chekov integrate`.
# The repo root is derived, not hardcoded — clone anywhere; `make install`
# wires that location into ~/.zshrc. Set CHEKOV_HOME if you move it later.

ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
ZSH_LINE := source $(ROOT)/shell/chekov.zsh

.PHONY: setup update install test lint deny

setup:
	cargo build --locked --release
	./target/release/chekov setup

update:
	chekov update --all

install:
	cargo install --path .
	chekov completions zsh > shell/_chekov
	grep -qF "$(ZSH_LINE)" ~/.zshrc || echo "$(ZSH_LINE)" >> ~/.zshrc

test:
	cargo test --locked

lint:
	cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings

deny:
	cargo deny check
