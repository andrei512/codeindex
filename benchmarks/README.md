# Agent benchmark

Run the same public Terminal-Bench 2.0 task with Codex in two fresh Docker environments. The baseline gets the task prompt. The treatment also gets the cx CLI and its checked-in introduction.

    python3 benchmarks/terminal_bench.py fix-git --model openai/MODEL --cx-binary /path/to/linux/cx --attempts 3

Use a model available to your Harbor Codex agent. The runner requires uvx, Docker, working Codex authentication, and an executable Linux build of cx matching the container architecture. Add `--dry-run` to inspect both commands without launching trials.

Harbor retains trajectories, commands, verifier results and logs under the ignored `target/terminal-bench/` directory. Each arm gets a JSON summary with reward, start and finish times, tool-call count, and Harbor's reported token and cost metrics. Missing metrics remain null. Compare usage alongside verifier reward. Raw runs stay local.

The other scripts here measure cx build performance and CLI parity.
