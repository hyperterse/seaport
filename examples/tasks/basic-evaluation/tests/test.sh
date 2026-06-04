#!/bin/bash
set -euo pipefail

mkdir -p /logs/verifier

if [ "$(cat /app/output.txt 2>/dev/null)" = "hello seaport" ]; then
  echo 1 > /logs/verifier/reward.txt
else
  echo 0 > /logs/verifier/reward.txt
fi
