# Distribution implementation status

Updated before the 0.6.0 release source freeze on 2026-09-14.

Overall progress: **65% of the amended Linux 0.6.0 release objective**. The first candidate has passed all five installation targets. Final source checks have passed. Release commit, final rebuild/retest and publication remain.

- Baseline: `b4d2ac929dbfa547c9e85468dfde8649f5228f88` (main).
- Worktree: `/home/shorin/Documents/github/Miyu-distribution-2026-09-14`.
- Branch: `worktree-distribution-2026-09-14`.
- Initial candidate: `out/distribution/linux-candidate-01/`, snapshot `872ba3b84af666399e9e25fb68d753ecee64fee462308d87edc7dd86c8eae8bf`.
- No commit, tag or publication has been performed at this document snapshot. The user authorizes release 0.6.0 after automated acceptance without waiting for manual review. Do not directly merge main or upgrade production Miyu.

## Acceptance scope

`linux-smoke` freezes five Linux x86_64 installation targets, core + voice and eight package assets including GNU tar. Core acceptance requires real dependency-resolving installation, artifact identity, complete resources and a successful nonempty `opencodego/deepseek-v4.1-flash` reply. Voice requires actual installation and matching identity/version. GNU tar uses its own relocated prefix and isolated home.

The original T00–T24 six-target plan remains an audit reference, not a claim of completed Mac, microphone/TCC, managed-service, upgrade or signing verification. macOS has no formal asset in this release. No missing hardware check is recorded as PASS.

## Actual evidence before final rebuild

All paths below are relative to `out/distribution/`.

| Check | Result | Evidence |
|---|---|---|
| Initial full refactor gate | PASS, 2,337 source/integration tests | `refactor-0.6.0-final-gates/report.json` |
| MSRV 1.89.0 | PASS, `cargo check --locked --all-targets` after three API alias corrections | `msrv-1.89-fixed/report.json`; original E0658 failure retained in `msrv-1.89/` |
| Atomic scheduling/image tests | PASS, 14 existing tests | `msrv-atomic-tests/report.json` |
| Final source gate after API corrections | PASS, 2,337 tests and all gates | `refactor-0.6.0-release/` |
| Python distribution suite | PASS, 66 tests | `packaging-tests-final.txt`; mutation/negative evidence described in [gate-repairs.md](gate-repairs.md) |
| GNU and Arch core/voice | Four real release builds PASS with networking disabled | `linux-candidate-01/build/*/*/build-record.json` |
| Debian 13 and independent GNU tar | PASS, 12 checks | `linux-candidate-01/reports-retry/debian13-x86_64/report.json` |
| Ubuntu 25.10 | PASS, 6 checks | `linux-candidate-01/reports-retry/ubuntu2510-x86_64/report.json` |
| Ubuntu 26.04 | PASS, 6 checks | `linux-candidate-01/reports/ubuntu2604-x86_64/report.json` |
| Fedora 44 | PASS, 6 checks | `linux-candidate-01/reports-retry/fedora-current-x86_64/report.json` |
| Arch snapshot 2026/09/13 | PASS, 6 checks | `linux-candidate-01/reports/arch-x86_64/report.json` |
| OOBE screenshots | Four real PTY captures, isolated network/home, processes removed | `docs/releases/0.6.0/oobe/` in the source tree |
| CI | Four workflows pass actionlint; CLI interfaces, dry-run and transfer permissions checked | `ci-check/final-static-report.json`; remote Actions execution not claimed |

The candidate results describe their recorded bytes. They are not reusable as final release evidence after source changes. The release pipeline must rebuild all four binaries, package all eight assets and repeat all five installed targets against one clean tagged source snapshot.

## Repairs discovered by actual execution

- A fixed reply-token assertion exceeded the user's normal-output acceptance. A valid personality response was preserved as evidence, then the probe was corrected to require successful final JSON, the requested provider/model and nonempty text.
- Fedora's filesystem owns `/usr/bin` and `/usr/lib` with mode 0555. RPM packages no longer claim shared root directories as 0755. Package payload and complete installed resource inventories are checked separately.
- Ubuntu 25.10 had not migrated to old-releases at testing time. Its original official archive/security sources returned 200; the guessed rewrite returned 404 and was removed.
- Actual namcap/readelf checks found Python missing for bundled scripts and bzip2 missing for Arch voice. All Arch recipes and GNU core dependency declarations were corrected. GNU voice does not link libbz2. Arch packaging now rejects namcap E diagnostics; final packaging/install must validate the corrected declarations.
- Three uses of unstable `try_update` were replaced with its stable, semantically identical `fetch_update` target. Ordering and closures are unchanged; MSRV remains 1.89.
- Publication now rejects unexpected remote draft assets and binds the complete frozen input. Actual builder image/compiler/command/binary evidence survives stage/package into provenance. A public acceptance JSON includes selected real replies and cleanup results without credentials or host commands.

## Completion sequence and cleanup

Complete the final source gate, record the release commit/tag, then create release-mode metadata and prepare from verified input caches. Rebuild, package and repeat all target probes. Only final package hashes with complete successful checks may enter aggregation and publication. Read uploaded bytes back before finalizing the draft. Update repository AUR truth sources from actual published hashes, without implicitly pushing a separate AUR repository.

Every product test uses an owned MIYU_HOME. Credentials remain in restricted temporary configuration outside source and artifacts. Confirm containers are gone before removing their bind-mounted homes. Remove task-owned containers, images and expendable build/download caches after publication; preserve final assets, required evidence and the worktree. Do not prune pre-existing Docker resources, other worktrees or production state.
