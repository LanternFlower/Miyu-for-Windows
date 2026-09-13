# Release gate repairs

Status at 2026-09-14 handoff: overall Linux 0.6.0 progress **35%**. The final full refactor script passed, including 2,337 nonignored source/integration tests and all later gates. The Python distribution suite passed 55 tests before the latest two installation regressions; the updated installation test module passed all six tests. These checks do not establish original Mac/hardware acceptance.

The first complete isolated refactor gate ran 2,334 nonignored library tests and found eight failures. The second run passed all 2,337 total nonignored source/integration tests after these repairs (the test count grew from the stale 2,113 baseline). Its later model-language/size/dependency gates exposed stale baseline data, recorded separately below.

- Two presentation assertions hardcoded Chinese despite the actual locale selecting English. Assertions now use the same locale-aware labels, preserving folding/content checks.
- Unknown-config tests used OOBE fields that are now recognized. They now use genuinely future fields. The retired top-level default_mode field follows the existing unknown-field preservation contract; retired plugin blocks remain ignored.
- Two memory reset fixtures used nonexistent sessions, rejected by current ownership validation. Fixtures now create real sessions and still assert that unrelated memory survives.
- The registry shape fixture omitted the existing send_subagent_message tool and the current load_skill shape. Refreshed against the actual unchanged tool catalog. No tool schema or prompt bytes were modified.
- The new subprocess harness acted as a Linux subreaper but only reaped after the suite, retaining orphan zombies while a timeout test checked their disappearance. It now reaps adopted exited children during execution, protecting its direct and pre-existing children. A real fork regression covers this. A separate regression ensures tee /dev/stderr cannot truncate earlier logs.
- The language gate mistook the exact built-in ledger account names for instructional prose. Added only ledger.account.description to its existing functional-data exceptions, with the source in src/ledger/books.rs. Product descriptions remain unchanged.
- Size and dependency baseline snapshots dated from before already-merged main features. Refreshed both from a temporary export of **unmodified main b4d2ac929dbfa547c9e85468dfde8649f5228f88**, not this worktree's changes. Existing debt remains documented (including src/render/stream/timeline.rs above the red line); current changes must still pass no-growth/no-new-edge checks. No threshold was weakened.

Evidence: `out/distribution/refactor-0.6.0/`, `refactor-0.6.0-fixed/` and `source-gate-fixes/` preserve the earlier runs. The final rerun is `out/distribution/refactor-0.6.0-final-gates/report.json` (`exit_code: 0`, `timed_out: false`) with `refactor.log`; it records 2,337 cases and successful final size/dependency gates. Generated `test_scripts/.test-count` is an intentional gate output and included in review.

## Publication guard regressions

Read-only fixtures reproduced two publication defects before any fix or remote operation:

- `lib/github_release.py` accepted an existing draft containing an extra, unverified macOS archive. It uploaded the local Linux allowlist and finalized the draft with both assets. The publisher now rejects extra/duplicate remote names before uploading, requires the complete exact allowlist after uploads and before finalization, then checks that allowlist again after finalization. An already-public release must already have the exact set. Interrupted uploads remain resumable drafts. A concurrent change during finalization is detected as failure; the GitHub API does not provide an atomic transaction over the asset set and draft state.
- `lib/release_bundle.py` compared only selected identity fields. A valid manifest with a different `wiki_commit` still accepted the old bundle. The verifier now requires exactly one input record with the expected filename, binds its checked file hash to `release_input_sha256`, and compares the entire parsed input through canonical JSON with the supplied frozen manifest. Updating the bundled input and recomputing all checksums cannot hide a mismatch with that manifest.

Tests were added first to `packaging/ci/tests/test_release.py`. Against the unchanged implementation, nine assertions failed across six new test methods, proving the missing checks. After the fixes, all 11 tests in that module passed. Cases cover pre-existing extra draft/public assets, an asset inserted during upload or finalization, changed caller input, rehashed embedded input, missing/duplicate input records and an incorrect input hash. All remote interactions used `FakeGitHub`; temporary fixtures were removed. No real release was uploaded or finalized.

Run the focused verification from the worktree root:

```bash
PYTHONPATH=packaging/ci:packaging/ci/tests python3 -B -m unittest test_release -v
```

## Installed-container findings and repairs

- Container removal errors were previously ignored before deleting the mounted test home. A fixture made only `docker rm -f` return nonzero and still received a PASS report. The verifier now records `cleanup.json`, confirms the owned container is absent, and only then removes its home. Failed or inconclusive cleanup fails acceptance; uncertain absence retains the restricted home. Four cleanup tests cover transport failure, a still-live container, removal failure followed by confirmed absence, and successful cleanup.
- The initial Debian target failed because the independent GNU tar probe returned the persona's normal reply `何意味` instead of the imposed `MIYU_DIST_OK` token. This was a successful provider response, not a transport failure. The exact-token requirement exceeded the user's normal-output acceptance and has been removed; process success, final response structure, nonempty text and expected provider/model remain checked. The DEB core separately did return `MIYU_DIST_OK`, with 195 inventory entries checked. The retry completed all 12 Debian checks, including independent tar homes, in `out/distribution/linux-candidate-01/reports-retry/debian13-x86_64/report.json`. Ubuntu 26.04 also completed all six checks in `reports/ubuntu2604-x86_64/report.json`.
- Fedora's first real install rejected `/usr/bin` and `/usr/lib` from the generated RPMs because the existing `filesystem` package owns those roots with mode 0555, while nFPM explicitly declared 0755. `package.py` now omits RPM ownership of shared roots (`bin`, `lib`, `share`, `share/licenses`) while retaining private directories and files. A separate `package_files` inventory describes the actual archive payload; installed resource checks retain the complete resource inventory. Initial conflict evidence remains in `out/distribution/linux-candidate-01/reports/fedora-current-x86_64/install-1.txt`. Regenerated RPM acceptance is running; no Fedora PASS is claimed yet.
- Ubuntu 25.10's first attempt rewrote working image sources to `old-releases.ubuntu.com` based on an assumed archive migration. The actual old-releases probe returned 404 while official archive/security probes returned 200. The verifier now retains the image's configured sources. The failed attempt remains in `out/distribution/linux-candidate-01/reports/ubuntu2510-x86_64/install-1.txt`. Retry acceptance is running; no Ubuntu 25.10 PASS is claimed yet.

The two installation fixes have focused regressions in `packaging/ci/tests/test_verify.py`; all six tests in that module passed after the changes. The 55-test full-suite result predates these two new cases and is not relabeled as a later full-suite run. At this handoff no commit, tag or publication has been performed for the distribution work.

## Rust 1.89 MSRV repair (50% progress checkpoint)

The real `cargo +1.89.0 check --locked --all-targets` run failed with E0658 (`atomic_try_update`) at `src/platforms/scheduling.rs:86` and `src/tools/vision/reference.rs:168,287`. Its failed report and compiler output remain in `out/distribution/msrv-1.89/`.

The installed official Rust 1.89.0 source establishes exact equivalence: `core/sync/atomic.rs:3441–3453` defines unstable `try_update` as `self.fetch_update(set_order, fetch_order, f)`. `fetch_update` is stable since Rust 1.45.0 (line 3375). Both use the same compare-exchange retry loop and return the same previous value or failure. The evidence excerpt, including its official documentation URL, is retained at `out/distribution/msrv-1.89-fixed/atomic-api-source.txt`.

Only the three method names changed to `fetch_update`; all `AcqRel`/`Acquire` orderings, quota closures, result handling and cancellation paths remain unchanged. The declared MSRV is still 1.89. `cargo fmt` completed. Reusing `out/distribution/msrv-1.89/target`, the real Rust 1.89.0 all-targets check now exits 0 without timeout; see `out/distribution/msrv-1.89-fixed/report.json` and `msrv.log`.

Existing scheduling tests passed 13/13, including the running/queued quota limit, and `context_images_reuse_resolved_ids_and_duplicate_content_cache` passed 1/1. Both ran under an owned `MIYU_HOME` sandbox and `ProcessSupervisor`, without timeout; the report and logs are in `out/distribution/msrv-atomic-tests/`. The sandbox and supervised processes were cleaned afterward. A new full refactor run after this source change will be recorded separately from the earlier 2,337-test result.

## Publication adapter and channel metadata observed after source freeze

The application release remains tagged at `bc7087f4c4b03adcef7f8cc8fce64a1c1abb0aaa`; its binaries, package bytes and 36 installed checks were unchanged by these delivery-tool corrections. The follow-up packaging commit records them separately rather than moving the published tag.

- GitHub REST `/releases/tags/v0.6.0` returned HTTP 404 for the created draft, while the authenticated `/releases` list returned draft ID 387984893. The adapter now uses a complete paginated authenticated list only after the tag endpoint returns 404, still rejects authentication/network errors, and rejects ambiguous tag matches. Three new regression assertions failed before the fix; the updated full Python suite passes 70 tests. Upload resumed the existing draft, then read back all 21 assets and finalized the release.
- `makepkg --printsrcinfo` rejected a read-only bind directory because BUILDDIR/PKGDEST defaulted there. An actual non-root container reproduced exit 11. Channel generation now points its temporary output directories and HOME at container `/tmp`; both generated .SRCINFO files then passed in the same read-only mount. Published asset hashes were downloaded and verified before applying the recipes to this worktree. Existing AUR checkouts with local changes were preserved.

## Public attachment scope correction

The user requested only distribution packages on the Release download page. The publisher had
used every file in the complete verification bundle as its upload allowlist. A new regression
failed against that implementation: internal JSON, SPDX, checksums, PNG and GNU tar files were
all uploaded alongside the six native packages.

Public names now come only from frozen assets with formats `archlinux`, `deb` and `rpm`.
Both the publisher and dry-run use that selection. The publisher still verifies the entire
bundle before any remote action; internal evidence tampering blocks publication. Existing
unknown or internal remote attachments are rejected, as are extra assets introduced during
upload or finalization. The complete Python suite passes 73 tests, including three new
regressions and the existing retry/hash-conflict checks using complete bundle fixtures.
Temporary fixture directories are cleaned by the tests. This correction makes no remote
changes by itself and does not alter package bytes or the frozen build matrix.
