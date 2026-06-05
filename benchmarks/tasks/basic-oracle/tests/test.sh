#!/bin/bash
set -euo pipefail

app_dir="${APP_DIR:-/app}"
logs_dir="${LOGS_DIR:-/logs/verifier}"

mkdir -p "$logs_dir"

if [ "$(cat "$app_dir/output.txt" 2>/dev/null)" = "hello seaport" ]; then
  echo 1 > "$logs_dir/reward.txt"
else
  echo 0 > "$logs_dir/reward.txt"
fi
