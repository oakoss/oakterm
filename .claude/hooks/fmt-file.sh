#!/bin/bash
# Formats after edit/write; same writer split as lefthook.yml. Failure exits 1:
# surfaced in the transcript, deliberately not exit 2 — the edit has already
# landed (PostToolUse cannot block it) and 2 would only feed the error back to
# Claude and trigger a correction loop.
INPUT=$(cat)
FILE=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty') || {
  echo 'fmt-file.sh: jq failed to parse hook input' >&2
  exit 1
}

if [ -n "$FILE" ]; then
  case "$FILE" in
    *.md) rumdl fmt "$FILE" ;;
    *) dprint fmt --allow-no-files "$FILE" ;;
  esac || exit 1
fi
exit 0
