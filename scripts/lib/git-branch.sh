#!/usr/bin/env bash

# shellcheck shell=bash

git_branch_current() {
  git rev-parse --abbrev-ref HEAD
}

git_branch_require_clean_tree() {
  if [[ -n "$(git status --porcelain)" ]]; then
    pp_error "Working tree is dirty. Commit, stash, or revert changes before running the build loop."
    git status --short >&2
    return 1
  fi
}

git_branch_prepare() {
  local select_branch="$1"
  local create_branch="$2"
  local start_ref="$3"

  git_branch_require_clean_tree || return 1

  if [[ -n "$select_branch" ]]; then
    if git show-ref --verify --quiet "refs/heads/$select_branch"; then
      pp_cmd "git switch $select_branch"
      git switch "$select_branch"
      return 0
    fi

    local matches
    matches="$(git branch -r --list "*/$select_branch" | sed 's/^[[:space:]]*//' || true)"
    if [[ -z "$matches" ]]; then
      pp_error "Branch not found locally or uniquely remotely: $select_branch"
      return 1
    fi
    if [[ "$(printf '%s\n' "$matches" | wc -l | tr -d ' ')" != "1" ]]; then
      pp_error "Remote branch name is ambiguous: $select_branch"
      printf '%s\n' "$matches" >&2
      return 1
    fi
    pp_cmd "git switch --track $matches"
    git switch --track "$matches"
    return 0
  fi

  if [[ -n "$create_branch" ]]; then
    if git show-ref --verify --quiet "refs/heads/$create_branch"; then
      pp_error "Local branch already exists: $create_branch"
      return 1
    fi
    pp_cmd "git switch -c $create_branch $start_ref"
    git switch -c "$create_branch" "$start_ref"
  fi
}

git_branch_push_current() {
  local remote="${1:-origin}"
  local branch
  branch="$(git_branch_current)"
  pp_cmd "git push -u $remote $branch"
  git push -u "$remote" "$branch"
}
