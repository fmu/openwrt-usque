#!/usr/bin/env bash
# Fetch CI artifacts for on-router testing.
#
#   tests/fetch-artifacts.sh build     # binary tarball from build.yml (usque-armv7-musl)
#   tests/fetch-artifacts.sh package   # .apk feed from package.yml
#   tests/fetch-artifacts.sh list      # show recent runs
#
# Artifacts land under tests/artifacts/<kind>/.
set -uo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib.sh"
KIND="${1:-list}"
REPO=fmu/openwrt-usque

case "$KIND" in
  list)
    for wf in build.yml package.yml; do
      echo "== $wf"
      gh run list --repo "$REPO" --workflow "$wf" --limit 8 \
        --json databaseId,status,conclusion,event,headSha,createdAt,displayTitle 2>&1 \
        | sed 's/^/  /' | head -80
    done
    ;;
  build|package)
    if [ "$KIND" = build ]; then WF=build.yml; NAME=usque-armv7-musl; DEST="$ART_DIR/build"
    else WF=package.yml; NAME=; DEST="$ART_DIR/pkg"; fi

    RUN_ID=$(gh run list --repo "$REPO" --workflow "$WF" --status success --limit 1 \
              --json databaseId --jq '.[0].databaseId' 2>/dev/null)
    if [ -z "${RUN_ID:-}" ] || [ "$RUN_ID" = null ]; then
      echo "no successful $WF run found (yet)" >&2
      gh run list --repo "$REPO" --workflow "$WF" --limit 5 2>&1 | sed 's/^/  /' >&2
      exit 2
    fi
    echo "downloading $WF run $RUN_ID -> $DEST"
    rm -rf "$DEST"; mkdir -p "$DEST"
    if [ -n "$NAME" ]; then
      gh run download "$RUN_ID" --repo "$REPO" -n "$NAME" -D "$DEST" || exit 3
    else
      gh run download "$RUN_ID" --repo "$REPO" -D "$DEST" || exit 3
    fi
    echo "run id: $RUN_ID" | tee "$DEST/RUN_ID.txt"
    find "$DEST" -type f | sed 's/^/  /'
    ;;
  *) echo "usage: $0 [list|build|package]" >&2; exit 1 ;;
esac
