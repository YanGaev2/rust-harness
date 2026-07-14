# Interface setup and DeepSeek v4-pro validation

## Required interface behavior

- `harness` must open the terminal interface directly, like `claude` or `codex`.
- If no provider is configured yet, the interface must still open.
- Provider setup must happen inside that interface, through an interface command such as `/provider add`, not as an external command the user has to copy from an onboarding error and not as a provider wizard before the interface opens.
- After provider setup succeeds, the same interface should be able to continue into a working REPL session.

## Required DeepSeek v4-pro validation

- The harness must be tested against DeepSeek v4-pro.
- The test must run through the harness agent flow, not by calling tools manually.
- The model should be asked to perform a simple file workflow:
  - create a folder;
  - create several files in that folder;
  - put small functions into those files;
  - find a file that was created before the test prompt.
- The full trace must be saved.
- The trace must include all model-visible tool calls and all tool results.
- Tool correctness must be reviewed after the run:
  - which tools were called;
  - whether the selected tools matched the task;
  - whether arguments were valid;
  - whether the tool results succeeded;
  - whether the model recovered from any tool errors.
- If tool errors occur, they must be recorded separately in a dedicated errors file.

## Expected artifacts

- A full trace file for the DeepSeek v4-pro run.
- A separate tool-error report when any tool call fails.
- A short validation note summarizing whether the harness can complete the workflow through DeepSeek v4-pro.

## Continuation scope

- Continue implementing the remaining project scope after the interface setup flow and DeepSeek trace validation are in place.
