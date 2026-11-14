#!/usr/bin/env bash
# Runs Claude Code on this PC for the issues in the "Ready (local)" column of
# the Family HQ board (users/ahmed-marzook/projects/3), instead of in GitHub
# Actions. Use it for work that needs this machine: Docker, the local stack,
# a warm Rust build cache.
#
#   scripts/claude-local.sh            # one pass over the column, then exit
#   scripts/claude-local.sh --watch    # keep polling, every 5 minutes
#   scripts/claude-local.sh --watch 10 # ... every 10 minutes
#
# For each issue it moves the card to In progress, makes a fresh worktree on
# a `claude/issue-N-...` branch from origin/main, and runs Claude headless
# there. Claude pushes the branch and opens the PR itself; from then on the
# usual CI, Claude review and auto-merge apply (automerge.yml matches the
# `claude/` prefix, since these PRs are opened as the owner, not the App).
#
# Nothing on GitHub can start this: it only pulls work, so a public repo
# can't run code on this PC. Needs `gh` logged in with the `project` scope,
# the Claude CLI (or the VS Code extension's copy), cargo and sqlx-cli.
#
# Settings (environment): CLAUDE_BIN, CLAUDE_LOCAL_DIR (default
# ../family-hq-agent next to the repo), MAX_TURNS (default 80), and
# CLAUDE_LOCAL_DOCKER=1 to let Claude use `docker compose` (see below).

set -euo pipefail

OWNER=ahmed-marzook
REPO=ahmed-marzook/family-hq
PROJECT_NUMBER=3
PROJECT_ID=PVT_kwHOApN33s4BrVm8
STATUS_FIELD=PVTSSF_lAHOApN33s4BrVm8zhpoxqM
COLUMN="Ready (local)"
MAX_TURNS=${MAX_TURNS:-80}

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${CLAUDE_LOCAL_DIR:-"$(dirname "$ROOT")/family-hq-agent"}
mkdir -p "$WORK/logs"

log() { echo "$(date '+%F %T') $*"; }

find_claude() {
  if [ -n "${CLAUDE_BIN:-}" ]; then echo "$CLAUDE_BIN"; return; fi
  if command -v claude >/dev/null; then command -v claude; return; fi
  # The VS Code extension ships its own CLI but doesn't put it on PATH.
  ls -d "$HOME"/.vscode/extensions/anthropic.claude-code-*/resources/native-binary/claude* \
    2>/dev/null | sort -V | tail -1
}
CLAUDE=$(find_claude)
[ -n "$CLAUDE" ] || { log "Claude CLI not found; set CLAUDE_BIN"; exit 1; }

# Option IDs are looked up by name, so renaming or reordering other columns
# doesn't break this.
option_id() {
  gh project field-list "$PROJECT_NUMBER" --owner "$OWNER" --format json --jq \
    ".fields[] | select(.id == \"$STATUS_FIELD\") | .options[] | select(.name == \"$1\") | .id"
}

set_status() {
  gh project item-edit --id "$1" --project-id "$PROJECT_ID" \
    --field-id "$STATUS_FIELD" --single-select-option-id "$2" >/dev/null
}

# Tools Claude may use without asking. Anything else is refused in headless
# mode, so this is the whole of what it can do on this PC. `cargo run` and
# arbitrary shell are left out on purpose. `git push` is added per issue,
# for that issue's branch only: the owner is exempt from main's branch
# protection, so a bare `git push` could land on main.
TOOLS="Edit,Write,Read,Glob,Grep"
CMDS=("cargo fmt" "cargo clippy" "cargo test" "cargo build" "cargo check"
  "cargo sqlx" "npm ci" "npm run build"
  "git status" "git diff" "git log" "git add" "git commit"
  "gh issue view" "gh issue comment" "gh pr create" "gh pr view" "gh pr diff")

# Docker is opt-in, and only as `docker compose` subcommands: bare `docker`
# would allow `docker run --privileged -v /:/host`, i.e. root on this PC, for
# an agent that reads issue text anyone can write. Compose files can mount
# host paths too, so Claude may not edit them (or add an override file) in
# these runs.
DENY="Edit(compose*.y*ml),Write(compose*.y*ml),Edit(docker-compose*),Write(docker-compose*),Edit(deploy/**),Write(deploy/**)"
DOCKER_NOTE="Docker is not available in this run; if the issue really needs the full stack, say so in the PR or on the issue."
if [ "${CLAUDE_LOCAL_DOCKER:-}" = 1 ]; then
  CMDS+=("docker compose build" "docker compose up" "docker compose down"
    "docker compose logs" "docker compose ps")
  DOCKER_NOTE="docker compose (build, up, down, logs, ps) is available if the issue needs the full stack; use up -d and run down when finished. You may not edit compose files."
fi
for cmd in "${CMDS[@]}"; do
  TOOLS+=",Bash($cmd:*)"
done

# Puts a claimed card back in Backlog (not the column, or the next pass would
# retry it forever) and says why on the issue.
give_back() {
  local item=$1 n=$2 why=$3
  log "#$n: $why; moved back to Backlog"
  set_status "$item" "$BACKLOG" || log "#$n: couldn't move the card back"
  gh issue comment "$n" --repo "$REPO" --body \
    "The local Claude run stopped: $why. This is back in Backlog." >/dev/null || true
}

work_on() {
  local item=$1 n=$2
  local branch wt logf

  [ "$(gh issue view "$n" --repo "$REPO" --json state --jq .state)" = OPEN ] || {
    log "#$n is closed; skipping"; return 0; }

  # Claimed first, so a second poller (or the next pass) won't take it too.
  set_status "$item" "$IN_PROGRESS"

  # Seconds in the name, so a retry straight after a failure gets a new branch.
  branch="claude/issue-$n-$(date +%Y%m%d-%H%M%S)"
  wt="$WORK/issue-$n"
  logf="$WORK/logs/issue-$n-$(date +%Y%m%d-%H%M%S).log"
  log "#$n: starting on $branch (log: $logf)"

  # Everything after the claim is checked by hand rather than left to set -e,
  # which would abandon the card in In progress and end the whole pass.
  git -C "$ROOT" worktree remove --force "$wt" 2>/dev/null || true
  if ! { git -C "$ROOT" fetch -q origin main &&
    git -C "$ROOT" worktree add -q -b "$branch" "$wt" origin/main; } >>"$logf" 2>&1; then
    give_back "$item" "$n" "the git worktree couldn't be set up (log: \`$logf\` on the owner's PC)"
    return 0
  fi

  local ok=0
  (
    cd "$wt"
    # A scratch database per worktree, so the SQLx macros check new queries
    # and `cargo sqlx prepare` works, without touching the real family-hq.db.
    # One shared target dir keeps builds incremental across issues.
    export DATABASE_URL="sqlite:agent.db"
    export CARGO_TARGET_DIR="$WORK/target"
    # `&&`, not set -e: errexit is off inside a subshell that's part of `||`.
    cargo sqlx database setup &&
    "$CLAUDE" -p "Work on GitHub issue #$n in $REPO. Read it first with \`gh issue view $n\`." \
      --max-turns "$MAX_TURNS" \
      --allowedTools "$TOOLS,Bash(git push -u origin $branch),Bash(git push origin $branch)" \
      --disallowedTools "$DENY" \
      --append-system-prompt "You are running unattended on the repo owner's PC, picked up from the project board's '$COLUMN' column, which counts as the owner asking for this issue. You are in a fresh git worktree on branch $branch, made from origin/main; DATABASE_URL points at a migrated scratch database. Follow CLAUDE.md, especially its Agent workflow section. Check docs/ROADMAP.md for the current stage; if the issue asks for a later-phase feature, say so with gh issue comment and stop without a PR. Keep the change small and focused on this one issue. $DOCKER_NOTE Before pushing, run cargo fmt --check, cargo clippy --all-targets -- -D warnings and cargo test; run npm ci and npm run build in web/ if anything under web/ changed; if any SQL query changed, run cargo sqlx prepare -- --all-targets and commit .sqlx/. Never commit agent.db or any other .db file. When done, push with git push -u origin $branch and open the PR with gh pr create --base main --head $branch, with a body that starts with 'Closes #$n' and says what changed and how it was tested."
  ) >>"$logf" 2>&1 || ok=$?

  local pr
  pr=$(gh pr list --repo "$REPO" --head "$branch" --json number --jq '.[].number' || true)
  if [ -n "$pr" ]; then
    log "#$n: PR #$pr opened"
    set_status "$item" "$IN_REVIEW" || log "#$n: couldn't move the card to In review"
    git -C "$ROOT" worktree remove --force "$wt" || true
  else
    # The worktree and log stay for a look.
    give_back "$item" "$n" "Claude finished without opening a PR (exit code $ok; log \`$logf\` and worktree \`$wt\` on the owner's PC)"
  fi
}

one_pass() {
  IN_PROGRESS=$(option_id "In progress")
  IN_REVIEW=$(option_id "In review")
  BACKLOG=$(option_id "Backlog")

  gh project item-list "$PROJECT_NUMBER" --owner "$OWNER" --limit 1000 --format json --jq "
    .items[]
    | select(.status == \"$COLUMN\" and .content.type == \"Issue\" and .content.repository == \"$REPO\")
    | \"\(.id) \(.content.number)\"" |
  while read -r item n; do
    # A failure on one card mustn't stop the rest of the pass.
    work_on "$item" "$n" </dev/null || log "#$n: unexpected error"
  done
}

# One run at a time: a watch loop and a manual run would otherwise both pick
# the same card. The lock holds the PID so a crashed run's lock is reclaimed.
LOCK="$WORK/lock"
if ! mkdir "$LOCK" 2>/dev/null; then
  if kill -0 "$(cat "$LOCK/pid" 2>/dev/null)" 2>/dev/null; then
    log "another run is going (pid $(cat "$LOCK/pid")); exiting"
    exit 0
  fi
  log "reclaiming a stale lock"
fi
echo $$ >"$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

if [ "${1:-}" = "--watch" ]; then
  every=${2:-5}
  log "watching '$COLUMN' every $every minutes (Ctrl+C to stop)"
  while true; do
    one_pass || log "pass failed; trying again next time"
    sleep "$((every * 60))"
  done
else
  one_pass
fi
