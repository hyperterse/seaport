#!/bin/bash
set -euo pipefail

app_dir="${APP_DIR:-/app}"

mkdir -p "$app_dir"
printf "hello seaport\n" > "$app_dir/output.txt"
