# chekov zsh integration — PATH, cclocal alias, completions.
# Sourced from ~/.zshrc (line appended idempotently by `make install`).
# Nothing here spawns a subprocess; shell startup stays fast (§E.3).

CHEKOV_ROOT="${CHEKOV_HOME:-$HOME/personal_dev/chekov}"

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
