# Repository agent guidance

## Subagent delegation

- Do not use subagents unless the user explicitly requests delegation.
- Perform repository exploration, implementation, testing, and review in the primary agent.

## Temporary files

- Use `./tmp` within the repository instead of `/tmp` for temporary files and directories.

## Optimization records

- Consolidate selfhost optimization ideas, experiment summaries, adoption status, and Rust-to-selfhost migration candidates in `optimize_logs/SELFHOST_CANDIDATES.md`. Update existing entries when revisiting an idea; do not create additional selfhost candidate catalogs.
- Keep detailed artifacts separate and link them from the selfhost catalog. Record Rust-only investigations and implementation evaluations in individual reports under `optimize_logs/` and index them in `optimize_logs/README.md`.
- Continue adding a one-line entry for newly adopted optimizations to `SELFHOST_OPTIMIZATION_EXPERIMENTS_EVALUATION.md`.
