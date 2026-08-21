# chekov zsh integration — CHEKOV_HOME, PATH, cclocal alias, completions.
# Sourced from ~/.zshrc (line appended idempotently by `make install`).
# Nothing here spawns a subprocess; shell startup stays fast (§E.3).

# The repo checkout this file lives in is the chekov root unless the user
# already pinned one. Exporting it means `chekov` resolves the same root from
# any directory — the binary's built-in default is ~/.chekov.
: "${CHEKOV_HOME:=${${(%):-%x}:A:h:h}}"
export CHEKOV_HOME
CHEKOV_ROOT="$CHEKOV_HOME"

export PATH="$CHEKOV_ROOT/bin:$PATH"
alias cclocal="$CHEKOV_ROOT/bin/cclocal"

# Completions: `make install` generates shell/_chekov via the hidden
# `chekov completions zsh` subcommand. Two hookup paths:
#   - fpath, for shells whose compinit runs after this file is sourced
#   - direct source, when compdef already exists (e.g. oh-my-zsh ran first)
fpath=("$CHEKOV_ROOT/shell" $fpath)
if (( $+functions[compdef] )) && [[ -f "$CHEKOV_ROOT/shell/_chekov" ]]; then
  source "$CHEKOV_ROOT/shell/_chekov"
fi
