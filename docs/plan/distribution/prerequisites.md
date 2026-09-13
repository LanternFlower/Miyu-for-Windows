# Distribution prerequisites

Observed on 2026-09-14 in the dedicated distribution worktree. Full logs: `out/distribution/t00/`.

| Item | Actual observation | Consequence |
|---|---|---|
| Baseline | main `b4d2ac929dbfa547c9e85468dfde8649f5228f88`, version 0.5.0 | Newer than plan baseline; source entry points inspected |
| Main local changes | `next-release-note.md`, `todolist.md` | Left untouched |
| Host | Linux x86_64, 61 GiB RAM, initially 262 GiB free disk | Native Linux container testing available |
| Docker | Client/server 29.7.2, initially no local images | Record owned images/containers for later removal |
| GNU baseline | Debian 13 image `sha256:f324c7ff54321e8d9c588493a20244965938ce0aa50bbd1022d38010e9ffc4b1` | Actual container reports x86_64, Debian 13.6, glibc 2.41 |
| Default Rust | 1.99.0-nightly (2026-07-12) | Record only; distribution will use a pinned stable compiler |
| Installed stable compiler | 1.96.1 (`31fca3adb283cc9dfd56b49cdee9a96eb9c96ffd`) | Candidate build toolchain |
| Declared MSRV | 1.89, installed 1.89.0; metadata parses | Actual dependency/compiler compatibility check still required by T01 |
| Python | 3.14.7 | Scripts target Python >=3.11, standard library only initially |
| GitHub | Authenticated read access; self-hosted runner API returns empty list | Hosted runner availability/execution still unverified; no remote mutations authorized |
| Mac | No local native Mac available | Native compile, macOS 15 runtime, GUI/TCC/hardware checks pending |
| Apple credentials | `APPLE_DEVELOPER_ID_P12`, `APPLE_DEVELOPER_ID_PASSWORD`, `APPLE_TEAM_ID`, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID`, `APPLE_NOTARY_KEY` absent from local environment | Signing/notarization BLOCKED; remote secret availability not inferred |
| Runner configuration | `LINUX_X64_RUNNER`, `MACOS_ARM64_RUNNER` absent locally | Configure controlled hosted defaults and validate in T19 |
| Wiki upstream | PKGBUILD uses `SHORiN-KiWATA/Shorin-ArchLinux-Guide`, HEAD resolved to `af99c4ac22a1be849206639807577a51b9e12061` | Lock this source in T02, not a guessed repository name |

## T00 protection checks

Eight actual unittest checks passed: fresh distinct roots; child-only home/XDG isolation preserving Cargo/Rustup locations; wrong marker refusal; existing root cannot be adopted; root replacement/symlink refusal; interior symlink cannot delete outside data; real timed-out subprocess reaped while unrelated process survives; Linux detached daemon reaped; unknown/unfinished suites fail closed (some checks grouped in one test). Logs and implementation digest are recorded in `out/distribution/t00/report.json`.

The runner uses only predefined suites. All subprocesses have timeouts, an owned session and PID records. Linux subreaper cleanup examines only the harness's own adopted children and matches the exact isolated MIYU_HOME; it never signals by process name. Unimplemented suites return exit 3/BLOCKED. Source-unit success does not prove resource-dependent tests executed inference; installed package probes remain separate required checks.

## Cleanup contract

User requested test environment cleanup. Temporary homes/processes are reclaimed in `finally`. Container invocations use `--rm`; named build containers must be registered for cleanup. Once no longer needed, remove only this task's images and caches. Preserve the uncommitted worktree, candidate packages and necessary evidence for user acceptance. Do not run global Docker prune or touch other worktrees/production Miyu data.
