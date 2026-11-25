#!/bin/sh
# PreToolUse hook: block `git commit` while formatting or clippy checks fail.
# Exit status 2 blocks the tool call and feeds stderr back to Claude.

cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0

if ! out=$(cargo fmt --check 2>&1); then
    printf 'cargo fmt --check failed. Run `cargo fmt`, stage the result, and commit again.\n%s\n' "$out" >&2
    exit 2
fi

if ! out=$(cargo clippy --all-targets --quiet -- -D warnings 2>&1); then
    printf 'cargo clippy reported problems. Fix them and commit again.\n%s\n' "$out" >&2
    exit 2
fi

exit 0
