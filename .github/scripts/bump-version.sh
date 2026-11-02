#!/usr/bin/env bash
# Compute the next version for a `filer` release.
#
# Usage: bump-version.sh <current> <patch|minor|major> [explicit]
#
#   current   version currently in Cargo.toml, e.g. 0.1.0 or 0.2.0-rc.1
#   kind      which part to bump when no explicit version is given
#   explicit  exact version to use instead (a leading "v" is tolerated)
#
# Prints the new version (no leading "v") on stdout. Exits non-zero on bad input.
set -euo pipefail

current=${1:?usage: bump-version.sh <current> <patch|minor|major> [explicit]}
kind=${2:?usage: bump-version.sh <current> <patch|minor|major> [explicit]}
explicit=${3:-}

semver_re='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'

if [[ ! $current =~ $semver_re ]]; then
  echo "error: current version '$current' is not valid semver" >&2
  exit 1
fi

if [[ -n $explicit ]]; then
  explicit=${explicit#v}
  if [[ ! $explicit =~ $semver_re ]]; then
    echo "error: requested version '$explicit' is not valid semver (expected e.g. 1.2.3)" >&2
    exit 1
  fi
  echo "$explicit"
  exit 0
fi

base=${current%%-*}
base=${base%%+*}
IFS=. read -r major minor patch <<<"$base"
had_prerelease=
[[ $current == *-* ]] && had_prerelease=1

case $kind in
patch)
  # Bumping "patch" from a prerelease releases that prerelease instead of skipping it.
  [[ -n $had_prerelease ]] || patch=$((patch + 1))
  ;;
minor)
  minor=$((minor + 1))
  patch=0
  ;;
major)
  major=$((major + 1))
  minor=0
  patch=0
  ;;
*)
  echo "error: unknown bump kind '$kind' (expected patch, minor or major)" >&2
  exit 1
  ;;
esac

echo "$major.$minor.$patch"
