#!/usr/bin/env bash
# Prepares a KiraVR release: Looks at the commits since the last release,
# recommends a version, sets it in Cargo.toml and Cargo.lock, writes a release
# notes draft, then (each step after asking) commits, tags and pushes. The tag
# push starts the release build on GitHub, which ends in a draft release.
#
# Usage: scripts/release.sh [patch|minor|major|X.Y.Z] [--dry-run]
#
# How the recommendation works (from the commit subjects, Conventional Commits):
#   major  a "!" after the type (feat!: ...) or "BREAKING CHANGE" in the body,
#          something that stops older setups from working as before
#   minor  any feat: (new features, nothing breaks)
#   patch  only fix:, perf:, docs:, chore: and the like
# The first release just uses the version Cargo.toml already has.
#
# Env: KIRAVR_DEBUG=1 logs every step, NO_COLOR=1 disables colors.

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
kiravr_init release

DRY_RUN=0
REQUEST=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        patch|minor|major) REQUEST="$arg" ;;
        [0-9]*.[0-9]*.[0-9]*) REQUEST="$arg" ;;
        -h|--help) sed -n 2,17p "$0"; exit 0 ;;
        *) error "Unknown argument: $arg"; exit 2 ;;
    esac
done

cd "$KIRAVR_REPO_DIR"

confirm() {
    local answer
    printf '%s [y/N] ' "$1"
    read -r answer
    [[ "$answer" == [yY]* ]]
}

run() {
    if [[ "$DRY_RUN" == "1" ]]; then
        echo "${CYAN}(dry run)${RESET} $*"
    else
        "$@"
    fi
}

# Checks
branch="$(git rev-parse --abbrev-ref HEAD)"
[[ "$branch" == "main" ]] || { error "Releases are made from main, this is $branch."; exit 1; }
if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    error "There are uncommitted changes. Commit or stash them first:"
    git status --short --untracked-files=no >&2
    [[ "$DRY_RUN" == "1" ]] || exit 1
fi
git fetch --quiet origin main --tags || warn "Could not reach GitHub, using what is here"
if [[ -n "$(git rev-list HEAD..origin/main 2>/dev/null)" ]]; then
    error "GitHub has commits that are not here. Run git pull first."
    exit 1
fi

current="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
last_tag="$(git describe --tags --match 'kiravr-v*' --abbrev=0 2>/dev/null || true)"

# Commits since the last release, newest last.
if [[ -n "$last_tag" ]]; then
    range="$last_tag..HEAD"
    info "Changes since $last_tag (installed version files say $current)"
else
    # Everything since KiraVR split off ALVR.
    fork_point="$(git merge-base HEAD upstream/master 2>/dev/null || true)"
    range="${fork_point:+$fork_point..}HEAD"
    info "First KiraVR release. Changes since the fork from ALVR"
fi
mapfile -t subjects < <(git log --reverse --format='%s' "$range")
bodies="$(git log --format='%b' "$range")"

level=patch
declare -a feats=() fixes=() others=() breaking=()
for subject in "${subjects[@]}"; do
    [[ "$subject" == release:* ]] && continue
    type="${subject%%:*}"
    if [[ "$type" == *"!" ]]; then
        breaking+=("$subject")
    elif [[ "$type" == feat* ]]; then
        feats+=("$subject")
    elif [[ "$type" == fix* || "$type" == perf* ]]; then
        fixes+=("$subject")
    else
        others+=("$subject")
    fi
done
grep -q 'BREAKING CHANGE' <<<"$bodies" && breaking+=("(a commit body says BREAKING CHANGE)")
if (( ${#breaking[@]} )); then
    level=major
elif (( ${#feats[@]} )); then
    level=minor
fi

show() {
    local title="$1" color="$2"; shift 2
    (( $# )) || return 0
    echo "${color}${title} ($#)${RESET}"
    printf '  %s\n' "$@"
}
show "Breaking" "$BRED" "${breaking[@]}"
show "Features" "$GREEN" "${feats[@]}"
show "Fixes" "$CYAN" "${fixes[@]}"
show "Other" "" "${others[@]}"
(( ${#subjects[@]} )) || { warn "No commits since $last_tag, nothing to release."; exit 0; }

bump() {
    local IFS=. major minor patch
    read -r major minor patch <<<"${current%%-*}"
    case "$1" in
        major) echo "$((major + 1)).0.0" ;;
        minor) echo "$major.$((minor + 1)).0" ;;
        patch) echo "$major.$minor.$((patch + 1))" ;;
    esac
}

if [[ -z "$last_tag" ]]; then
    recommended="$current"
    reason="it is the first release, so it keeps the version already set"
else
    recommended="$(bump "$level")"
    case "$level" in
        major) reason="something breaks for existing setups" ;;
        minor) reason="there are new features and nothing breaks" ;;
        patch) reason="there are only fixes and small changes" ;;
    esac
fi
echo
if [[ -z "$last_tag" ]]; then
    echo "${BOLD}Recommended: $recommended${RESET} (because $reason)"
else
    echo "${BOLD}Recommended: $recommended${RESET} ($level, because $reason)"
fi

case "$REQUEST" in
    "") version="$recommended" ;;
    patch|minor|major) version="$(bump "$REQUEST")" ;;
    *) version="$REQUEST" ;;
esac
[[ "$version" == "$recommended" ]] || warn "Using $version instead of the recommendation, as asked"
if git rev-parse -q --verify "refs/tags/kiravr-v$version" >/dev/null; then
    error "kiravr-v$version already exists."
    exit 1
fi
confirm "Release KiraVR $version?" || { info "Nothing changed."; exit 0; }

# Version files. Cargo.lock lists every workspace crate with the version, so
# it is edited directly (no cargo needed on SteamOS).
if [[ "$version" != "$current" ]]; then
    info "Setting the version to $version"
    if [[ "$DRY_RUN" != "1" ]]; then
        python3 - "$current" "$version" <<'EOF'
import re, sys
old, new = sys.argv[1], sys.argv[2]
toml = open("Cargo.toml").read()
toml = toml.replace(f'version = "{old}"', f'version = "{new}"', 1)
open("Cargo.toml", "w").write(toml)
lock = open("Cargo.lock").read()
# Workspace crates have no "source" line.
blocks = lock.split("\n\n")
for i, block in enumerate(blocks):
    if 'name = "alvr_' in block and "source =" not in block:
        blocks[i] = block.replace(f'version = "{old}"', f'version = "{new}"', 1)
open("Cargo.lock", "w").write("\n\n".join(blocks))
EOF
    fi
    ok "Cargo.toml and Cargo.lock set to $version"
fi

# Release notes: The workflow puts docs/release-notes/<version>.md on top of
# the GitHub release. Written from the commits when there is none yet.
notes="docs/release-notes/$version.md"
if [[ ! -f "$notes" ]]; then
    info "Writing a release notes draft: $notes"
    if [[ "$DRY_RUN" != "1" ]]; then
        mkdir -p docs/release-notes
        {
            echo "## What's New"
            echo
            for s in "${breaking[@]}" "${feats[@]}"; do echo "- ${s#*: }"; done
            if (( ${#fixes[@]} )); then
                echo
                echo "## Fixes"
                echo
                for s in "${fixes[@]}"; do echo "- ${s#*: }"; done
            fi
        } > "$notes"
    fi
fi
echo "Read and edit ${BOLD}$notes${RESET} now if you like (it is what people see first)."
read -r -p "Press Enter when the notes are ready... " _

run git add Cargo.toml Cargo.lock "$notes"
if [[ "$DRY_RUN" == "1" ]] || ! git diff --cached --quiet; then
    run git commit -S -m "release: KiraVR $version"
fi
run git tag -s "kiravr-v$version" -m "KiraVR $version"
ok "Tagged kiravr-v$version"

if confirm "Push main and the tag now? This starts the release build on GitHub."; then
    run git push origin main
    run git push origin "kiravr-v$version"
    ok "Pushed. In about 30 to 60 minutes a draft release appears under Releases: Check it and click Publish."
else
    echo "Push later with: git push origin main && git push origin kiravr-v$version"
    echo "Undo with: git tag -d kiravr-v$version && git reset --hard HEAD~1"
fi
