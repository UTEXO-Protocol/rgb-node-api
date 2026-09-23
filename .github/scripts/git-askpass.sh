#!/bin/sh
# Used only for Git dependency fetches; never persist a token in a URL/config.
set -eu
case "$1" in
  *github.com*) ;;
  *) exit 1 ;;
esac
case "$1" in
  *Username*) printf '%s\n' 'x-access-token' ;;
  *Password*)
    if [ -n "${BFA_MIRRORS_TOKEN_FILE:-}" ]; then
      cat "$BFA_MIRRORS_TOKEN_FILE"
    else
      printf '%s\n' "${BFA_MIRRORS_TOKEN:?Git read access to the RGB mirrors is required}"
    fi
    ;;
  *) exit 1 ;;
esac
