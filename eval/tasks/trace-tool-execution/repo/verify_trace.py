#!/usr/bin/env python3
"""
Verify that the model correctly traced the tool execution path through nerv.

The complete path is:
  Agent::prompt (agent.rs)
    → stream_response / handle model output
    → execute_tools (agent.rs)
      → PermissionFn check (agent.rs, permissions.rs)
      → ToolRegistry::get (tool_registry.rs)
      → tool.validate() + tool.execute() (AgentTool trait)
      → post_tool_fn callback (agent.rs, bootstrap.rs)
    → results added to messages
    → loop continues

The answer must identify:
1. execute_tools as the entry point for tool dispatch
2. Permission checking (PermissionFn or permission)
3. ToolRegistry for tool lookup
4. AgentTool trait (validate + execute)
5. PostToolFn / post_tool_fn callback
6. The loop structure (agent continues after tool results)
"""

import sys
from pathlib import Path

ANSWER_FILE = "answer.md"


def main():
    if not Path(ANSWER_FILE).exists():
        print(f"FAIL: {ANSWER_FILE} not found")
        sys.exit(1)

    content = Path(ANSWER_FILE).read_text().lower()

    if len(content.strip()) < 300:
        print(f"FAIL: answer too short ({len(content.strip())} chars)")
        sys.exit(1)

    checks = []

    # 1. Must identify execute_tools
    if "execute_tools" in content or "execute_tool" in content:
        checks.append(("execute_tools", True))
    else:
        checks.append(("execute_tools", False))

    # 2. Must mention permission checking
    permission_terms = ["permissionfn", "permission_fn", "permission check", "permission prompt",
                        "permissions", "allowed", "check_permission"]
    if any(t in content for t in permission_terms):
        checks.append(("permissions", True))
    else:
        checks.append(("permissions", False))

    # 3. Must mention tool registry or dispatch
    registry_terms = ["toolregistry", "tool_registry", "registry.get", "registry"]
    if any(t in content for t in registry_terms):
        checks.append(("tool_registry", True))
    else:
        checks.append(("tool_registry", False))

    # 4. Must mention the AgentTool trait or tool.execute
    trait_terms = ["agenttool", "agent_tool", "tool.execute", "tool.validate",
                   ".execute(", "validate("]
    if any(t in content for t in trait_terms):
        checks.append(("agent_tool_trait", True))
    else:
        checks.append(("agent_tool_trait", False))

    # 5. Must mention post_tool_fn / PostToolFn callback
    post_terms = ["post_tool_fn", "posttool", "post_tool", "post-tool",
                  "symbol index update", "symbol_index", "after tool"]
    if any(t in content for t in post_terms):
        checks.append(("post_tool_fn", True))
    else:
        checks.append(("post_tool_fn", False))

    # 6. Must describe the loop / continuation
    loop_terms = ["loop", "continues", "next iteration", "next turn",
                  "back to", "repeats", "agentic loop", "agent loop"]
    if any(t in content for t in loop_terms):
        checks.append(("loop_structure", True))
    else:
        checks.append(("loop_structure", False))

    passed = [name for name, ok in checks if ok]
    failed = [name for name, ok in checks if not ok]

    for name, ok in checks:
        print(f"  {'OK' if ok else 'MISS'}: {name}")

    # Require at least 5 of 6 checks to pass
    if len(passed) >= 5:
        print(f"\nPASS: {len(passed)}/6 checks ({failed} missed)" if failed
              else f"\nPASS: {len(passed)}/6 checks")
        sys.exit(0)
    else:
        print(f"\nFAIL: only {len(passed)}/6 checks passed (need 5)")
        sys.exit(1)


if __name__ == "__main__":
    main()
