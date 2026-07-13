Execute the implementation plan at `docs/superpowers/plans/2026-07-13-token-usage-tracker.md` in this repo (`C:\Users\huynh\token-tracker`, GitHub remote `origin` = hdtinh57/token-usage-tracker, branch `master`). Read the full plan file first, then the spec it links to (`docs/superpowers/specs/2026-07-13-token-usage-tracker-design.md`) for background — the plan is self-contained (exact file paths, full code, full test code, exact commands, expected output for every step) but the spec explains *why* the tricky parts (truncation handling, day-rollover, repricing) work the way they do, in case you need to resolve an ambiguity the plan doesn't cover.

Run each of the 10 tasks in order, one subagent per task, model `gpt-5.6-terra`. Do not start task N+1 until task N's subagent reports its commit made and `cargo test` passed. Give each subagent only its own task's section from the plan (Files/Interfaces/Steps) plus this shared context:

- Global Constraints section from the top of the plan (dependency limits, no DB/service/file-watch-crate, timestamp/timezone rule, spec file path).
- The exact interfaces ("Consumes"/"Produces") of any earlier task it depends on, so it knows the real signatures instead of guessing.
- Working directory: `C:\Users\huynh\token-tracker`.

Each subagent must, in order: write the failing test(s) exactly as given in the plan step, run `cargo test` and confirm the new test(s) fail for the expected reason, write the implementation exactly as given, run `cargo test` and confirm everything passes (old tests included), then run the exact `git add`/`git commit` from the plan step. If `cargo test` fails for a reason other than "not yet implemented" (a real compile error, a version mismatch in the `eframe`/`egui_plot` API, etc.), the subagent should fix it directly — the plan's code is correct as of the design date but crate APIs can drift; fixing a compile error to match the currently-installed crate version is expected, not a deviation from the plan.

After task 10 (UI + main.rs wiring) is committed, do the manual smoke test described in that task's Step 4 yourself (`cargo run --release`, confirm the window opens, confirm `pricing.json` gets created next to the exe, confirm numbers update after using Claude Code or Codex CLI) and report the result — don't mark the plan done on tests alone, since Task 10 has no automated coverage.

Once all 10 tasks are committed and the smoke test passes, push to `origin master` and report the final commit hash.

If a task's tests reveal an actual bug in the plan's code (not a crate-version issue) — e.g. a logic error in the rollover or repricing math — stop and report it rather than silently patching around it; that means the design itself needs a second look, not just the implementation.
