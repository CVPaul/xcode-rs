## Task 2: Bash Tool + sbox Integration Progress

### BashTool Implementation
- Created `src/tools/bash.rs` with BashTool struct
- Supports both sandboxed (via SboxSession) and unsandboxed (direct shell) execution
- Output truncation logic implemented (configurable output_limit)
- Unit tests for direct shell execution and output truncation
- Integration with SandboxSession trait for future sbox support

### sbox CLI Usage (for reference)
- `sbox create <session_name>`: create sandbox session
- `sbox mount <session_name> <src> <dst>`: mount directory
- `sbox exec <session_name> -- <cmd>`: execute command in sandbox
- `sbox destroy <session_name>`: clean up session

### Coordination Notes
- BashTool currently uses direct shell execution; sbox logic is stubbed for future implementation
- ToolContext integration pending T4 landing
- All cargo tests pass for sandbox and bash modules
- No changes made to config.rs, llm/mod.rs, or llm/openai.rs as per constraints
- Awaiting ToolContext definition for full BashTool trait integration

### Next Steps
- Implement sbox exec logic in SboxSession when ToolContext is available
- Expand BashTool tests to cover sandboxed execution
- Monitor for T4/T5 merge conflicts and document any issues

## [T7] Agent Loop
- Agent trait in mod.rs, CoderAgent in coder.rs, OrchestratorAgent in orchestrator.rs, Director in director.rs
- truncate_messages() keeps messages[0] (system) + messages[1] (first user) + last N within budget
- MockLlmProvider pattern for tests: Vec<LlmResponse> queue, pop front on each call
- [add any gotchas encountered]
