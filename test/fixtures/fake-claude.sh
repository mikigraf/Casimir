#!/usr/bin/env bash
# Minimal stand-in for `claude -p --output-format stream-json`: echoes the prompt back, edits a file.
prompt=$(cat)
sid="fake-claude-session"
for ((i=1;i<=$#;i++)); do
  if [ "${!i}" = "--session-id" ] || [ "${!i}" = "--resume" ]; then j=$((i+1)); sid="${!j}"; fi
done
esc=$(printf '%s' "$prompt" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' ' ')
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$sid\",\"model\":\"fake-model\",\"cwd\":\"$PWD\"}"
echo "{\"type\":\"assistant\",\"session_id\":\"$sid\",\"message\":{\"id\":\"m1\",\"model\":\"fake-model\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Working on: $esc\"},{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Write\",\"input\":{\"file_path\":\"$PWD/out.txt\",\"content\":\"hi\"}}],\"usage\":{\"input_tokens\":10,\"output_tokens\":20}}}"
printf 'hi\n' > out.txt
echo "{\"type\":\"user\",\"session_id\":\"$sid\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}]}}"
echo "{\"type\":\"assistant\",\"session_id\":\"$sid\",\"message\":{\"id\":\"m2\",\"model\":\"fake-model\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Done.\"}],\"usage\":{\"input_tokens\":5,\"output_tokens\":5}}}"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"$sid\",\"total_cost_usd\":0.01,\"num_turns\":2,\"result\":\"Done.\",\"modelUsage\":{\"fake-model\":{}}}"
