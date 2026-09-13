# Distribution implementation decisions

The supplied 2026-09-13 execution plan fixes the six target IDs and T00–T24 scope. No target is removed to make checks pass. Progress is VERIFIED tasks / 25, separate from local implementation.

- Work on `worktree-distribution-2026-09-14`, based on main `b4d2ac92`. The user subsequently authorized committing and publishing version 0.6.0 after automated acceptance, without waiting for manual approval. Continue on this worktree; do not directly merge main.
- Use uncommitted preview snapshots with explicit new-source inclusion. Build/test evidence must bind both git commit and snapshot hash.
- Linux preview may exercise a declared target subset during development. This is not full completion. Stable profiles keep all six core targets and existing Linux voice.
- Freeze signing policy before building: preview Mac direct downloads are unsigned prereleases; stable direct-download Mac core/voice require signed-download verification. Homebrew-only unsigned core can be considered only as a separately declared, validated channel, not a runtime bypass of stable direct-download checks.
- Mac voice hardware/TCC and Apple signing remain required external validations. No Linux cross-compile, mock or missing-credential skip can satisfy them.
- Keep glibc 2.41 GNU build baseline, current Arch system ORT provider, pinned GNU private CPU ORT, thin LTO/codegen-units=1, existing user directory/database/prompt contracts.
- The repository has four PKGBUILDs (the older AGENTS build note says three). The plan and current source agree on preserving all four.
- Reuse current `test_scripts/refactor-check.sh` path. Its comments are stale; actual script and exit status are authoritative.

## User amendment (2026-09-14, authoritative over the original test bar)

The user changed acceptance to actual container package installation plus a successful real Miyu response using the existing `opencodego/deepseek-v4.1-flash` provider. After all applicable targets pass, publish a new 0.6.0 release autonomously. Review and fix release-process defects as needed. Include real OOBE screenshots in the release note. Test environments must be cleaned after use. Credentials must remain ephemeral and must not enter logs, source exports or artifacts.

The full implementation plan remains a reference, but its hardware/manual acceptance and no-publication constraints are superseded where they conflict with this explicit instruction. The Mac publication scope has been asked separately because Linux containers cannot supply native Apple Silicon evidence. No Mac success claim may be inferred from Linux tests.

### 0.6.0 Linux smoke release profile

Following the changed acceptance instruction, `linux-smoke` is an additional explicit profile. It freezes all five Linux installation targets and retains main + voice packages for Arch/GNU. Required core checks are artifact identity, real dependency-resolving installation, complete packaged resources and a successful real `opencodego/deepseek-v4.1-flash` response. Voice requires matching artifact identity and actual package installation. GNU tar assets are validated independently. No physical microphone, managed-service or native Mac claim follows from these smoke results. Original stable-core/stable-full profiles and their stronger checks remain intact.

Mac scope was asked asynchronously. Pending a different answer, proceed with the recommended Linux-only 0.6.0 asset set, with Mac adaptation explicitly unverified and unreleased.
