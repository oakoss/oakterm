#!/bin/bash
# Formats after edit/write; same writer split as lefthook.yml. Failure exits 1:
# surfaced in the transcript but non-blocking (only exit 2 blocks the edit).
INPUT=$(cat)
FILE=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')

if [ -n "$FILE" ]; then
  case "$FILE" in
    *.md) rumdl fmt "$FILE" ;;
    *) dprint fmt --allow-no-files "$FILE" ;;
  esac || exit 1
fi
exit 0
