# Distribution input inventory

Source baseline: `b4d2ac929dbfa547c9e85468dfde8649f5228f88`.
Task scope: T02/T03 input locks and resource inventory only.
Overall progress when prepared: 4% (T00 VERIFIED). These files do not mark all of T02/T03 VERIFIED.

## Verified upstream inputs

All URLs and SHA256 values in `packaging/common/third-party.lock.json` were obtained from actual downloads. The input directory is `out/distribution/input-inventory/` in this worktree. Each download has a `<archive-id>.json` receipt with URL, local path, size and SHA256. A second pass rehashed all 9 downloaded files successfully.

| ID | File inside input directory | Bytes | SHA256 |
|---|---|---:|---|
| `sherpa-linux` | `sherpa-onnx-v1.13.7-linux-x64-static-lib.tar.bz2` | 22,455,793 | `d1be7a69ac2b30120058d8302e624239a3064085383cfa47994a14fdc44c32d6` |
| `sherpa-macos` | `sherpa-onnx-v1.13.7-osx-arm64-static-lib.tar.bz2` | 20,339,537 | `126daa2e8c09a4c5d54dc985722c43bd22f598adc56445905b377454b1b27e38` |
| `ort-gnu` | `onnxruntime-linux-x64-1.23.2.tgz` | 8,309,231 | `1fa4dcaef22f6f7d5cd81b28c2800414350c10116f5fdd46a2160082551c5f9b` |
| `voice-kws` | `sherpa-onnx-kws-zipformer-wenetspeech-3.3M-2024-01-01.tar.bz2` | 32,654,866 | `b2f7c89690dc8ce4c6ed6afeab7cd800c36ad1421fb6b6302b4a4b194cf7f35f` |
| `voice-sense` | `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2` | 163,002,883 | `7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e` |
| `voice-vad` | `silero_vad.onnx` | 643,854 | `9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6` |
| `wiki` | `shorinwiki-af99c4ac22a1be849206639807577a51b9e12061.tar.gz` | 153,543,152 | `3d705588acf19cb28c9c1307c9b696feaffaf9c9b88412c5976af3d8015141d1` |
| `sherpa-onnx-license` | `sherpa-onnx-LICENSE` | 11,358 | `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30` |
| `silero-vad-license` | `silero-vad-LICENSE` | 1,075 | `2e63e9a38b6e8fc0c7bc37ce174caca1862870856c6daf5697cfb785e925520b` |

Total downloaded bytes: 400,961,749. The entire input directory belongs to this task, identified by `.miyu-owned-input-inventory`. Retain it until packaging/voice checks finish. Then remove this owned directory, including `ort-probe/` and `voice-fixtures/`. No user configuration, product home, daemon, Docker container or image was created by this subtask. No downloads were written outside the worktree.

## ORT selection evidence

`Cargo.lock` fixes `ort` and `ort-sys` at `2.0.0-rc.13`. The project disables defaults and requests `load-dynamic` and `std`. The dependency also disables `ort-sys` defaults. `ort-sys/src/version.rs` starts at API 17 and only increments for enabled `api-18` through `api-28`. None is enabled in this project. The required C API is therefore 17; the crate release number is not an ORT API number.

The fixed GNU candidate is Microsoft's CPU `onnxruntime-linux-x64-1.23.2.tgz`. A Python ctypes probe loaded the downloaded `libonnxruntime.so.1.23.2`, received version `1.23.2`, and received a non-null `GetApi(17)` result. Receipt: `ort-api-probe.json`. The extracted library is at `ort-probe/onnxruntime-linux-x64-1.23.2/lib/`. This proves API availability on this host. T05 must still prove embedding inference, complete ELF compatibility and CPU requirements inside the baseline environment.

The two sherpa archive versions are exactly the locked `sherpa-onnx-sys` version `1.13.7`. Linux SHA256 also matches the existing release PKGBUILD. Both archives contain static library inputs for their named architecture. This does not prove Mac compilation or microphone behavior.

## Voice offline fixtures

The locked KWS archive contains all three `*-epoch-12-avg-2-chunk-16-left-64.int8.onnx` files and `tokens.txt` required by `src/voice/models.rs`. The SenseVoice archive contains `model.int8.onnx` and `tokens.txt`. The separately locked `silero_vad.onnx` completes the product's seven required voice files. Full member hashes are in `voice-member-hashes.json`, and required models plus WAV member hashes are also in the checked-in lock's `fixtures` list.

Two public WAVs were actually extracted:

- `voice-fixtures/voice-kws-0.wav`, archive member `sherpa-onnx-kws-zipformer-wenetspeech-3.3M-2024-01-01/test_wavs/0.wav`, SHA256 `668bf8df51a10027b84d5d8816a1ce11ae93545538dc05cfe2aa6811d399c250`.
- `voice-fixtures/voice-sense-zh.wav`, archive member `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/test_wavs/zh.wav`, SHA256 `b77f1794fe374a0ba1ee1dc458bfaf9349496cbbfc32780c50ba3c5a7ad8e373`.

Both are mono, 16 kHz, 16-bit PCM. Their exact public download URL is their containing archive's locked URL. Downloading another mutable WAV is unnecessary. These materials are sufficient to prepare an offline voice test directory after safe archive extraction. No voice inference was executed in this subtask. Expected ASR transcript and KWS keyword assertions still need T18 probe implementation and verification.

## Resource rule contract

`packaging/common/assets.json` has schema version 1 and 21 rules. Every rule specifies `id`, `source_root`, `source`, `destination`, `mode`, `component` and `type`. Paths are relative to explicit roots. Destinations are prefix-relative. Data is `0644`; the 22 scripts are `0755`.

`type=tree` enumerates files recursively in sorted relative-path order. `include` globs match each file's basename at any depth. `exclude` entries reject a matching directory component at any depth. `required_files` are paths relative to the rule's source. `indexes` identifies meme indexes whose image references must be validated. `build_ids`, when present, restrict a rule to named build IDs; it is used for GNU private ORT only. Internal ORT symbolic links must be preserved and checked by stage, not dereferenced to duplicate libraries.

The source roots are:

- `source`: exported Miyu source snapshot.
- `wiki`: the exported root of fixed Wiki commit `af99c4ac22a1be849206639807577a51b9e12061`, retaining `LICENSE` and `wiki/` but excluding `.git`.
- `runtime`: prepared/generated data containing `onnxruntime/` (archive top directory stripped), `licenses/sherpa-onnx-LICENSE`, and generated `default-kb/manifest/{manifest.json,shorinwiki.commit}`.

The inventory reproduces the four actual PKGBUILDs: three fonts and their licenses, MIT application license in core and voice, seven embedding model files plus the separately installed model license, 36 meme images and one index, 22 scripts, and 40 local KB markdown files. KB/Wiki filters retain `*.md` and exclude `.git`, `pictures`, `legacy`, `Legacy`, `lagacy`, `Lagacy` and `Wikis`. Top-level model export scripts and READMEs are not installed, matching the original minimum-depth rule. Wiki content is taken only from the `wiki/` subdirectory. All source rules were checked nonempty; the model and tokenizer hashes match the source manifest. All 36 meme index image references exist. Evidence: `resource-inventory-checks.json`.

The binary and relative `miyupm -> miyu` link are stage responsibilities, since their input is the explicit component build root. Compiled-in prompts/web/word lists remain build inputs. They do not need duplicate external resource rules.

## Licenses and remaining work

GNU ORT's archive contains `LICENSE` and `ThirdPartyNotices.txt`; both have resource rules. The sherpa static archives contain no license. The fixed corresponding source commit's Apache-2.0 LICENSE is downloaded separately and has a voice package rule. T05/T18 must still inventory notices needed by the statically linked third-party dependencies.

The Wiki archive has a CC BY-SA 4.0 LICENSE, now explicitly installed as `share/licenses/miyu/ShorinWiki.LICENSE` (the old PKGBUILDs omitted it). The unchanged Wiki content retains its source commit manifest.

Voice models and WAVs are acceptance inputs; this inventory does not newly distribute them inside packages. The KWS model archive has no LICENSE, so the lock truthfully records `NOASSERTION`. SenseVoice's `LICENSE` is a 71-byte reference to the FunASR license section rather than a complete license. Silero's fixed upstream MIT license is locked separately. Redistribution of these test models requires resolving the upstream terms; this is not claimed verified by a successful download.

The initial inventory step did not claim T02/T03 verified. The T02 follow-up below now supplies preparation evidence. T03 still requires stage execution, missing font/license/meme negative tests, stable stage manifests and nonempty-destination rejection. Mac integration and installed package testing were not performed by this subtask.


## T02 implementation and actual execution follow-up

The parent subsequently assigned `prepare.py`, `lib/downloads.py`, `lib/inputs.py` and `tests/test_prepare.py`. Overall progress at handoff is 20% for the user-updated Linux 0.6.0 objective (the original 25-task matrix remains 4% fully VERIFIED).

`prepare.py` only consumes the manifest-adjacent exported `source/` and `source-files.json`. It checks the full snapshot inventory, file bytes/modes/links, all frozen locks and source SHA. Cached archives are rehashed even offline. Downloads are atomically saved. Tar paths, link cycles, link escapes, hardlink escapes and link-parent pivots are rejected before extraction. Runtime resources, exact Wiki SHA, models/WAVs and nFPM are prepared without executing Miyu. Cargo vendor is bounded by a 1,200-second timeout and then checked using Cargo metadata with `--locked --offline` against the generated vendor configuration.

`--skip-vendor` truthfully records `vendor_complete:false`. `--vendor-cache` accepts a prior complete prepared directory only when Cargo.lock matches and every vendor file plus configuration passes its stored SHA256. Files are copied, never hardlinked, and copied bytes are checked again. This permits different Miyu snapshots with identical dependency locks to reuse dependencies without claiming that their source snapshots match.

`lib/inputs.py::verify_prepared(inputs, manifest, manifest_path=None, require_vendor=False)` is shared by build/stage. It checks manifest SHA, source commit/snapshot, Wiki SHA, Cargo.lock and the complete actual inventory of files, modes and symbolic links. Missing, extra or changed files fail. Build can require complete vendor inputs explicitly.

Actual checks:

- The initial new negative-test module failed before `lib/downloads.py` existed. After implementation, all 12 tests pass: cache tampering, offline missing cache, successful offline cache use, archive absolute/traversal/link escape cases, internal ORT symlink chains, source snapshot mutation/undeclared files, lock/inventory mismatch, prepared-file mutation/extra files/wrong provenance, incomplete vendor, changed manifest, and vendor copy/tamper rejection.
- `python3 packaging/ci/prepare.py --manifest out/distribution/prepare-validation/release-input.json --out out/distribution/prepare-validation/inputs --cache out/distribution/input-inventory --offline --skip-vendor` passed with 287 files and an explicit incomplete-vendor result.
- `python3 packaging/ci/prepare.py --manifest out/distribution/prepare-validation/release-input.json --out out/distribution/prepare-full-inputs --cache out/distribution/input-inventory` passed with 31,173 inventory records and `vendor_complete:true`. This really ran Cargo vendor and Cargo's offline metadata check. It also included nFPM from the toolchain lock.
- Repeating complete prepare with `--offline --vendor-cache out/distribution/prepare-full-inputs` into a new owned directory passed, including another full 31,173-file verification. Receipt: `out/distribution/input-inventory/prepare-offline-reuse-check.json`.

The two already-generated preparation manifests gained the new `cargo_lock_sha256` field by revalidating their unchanged frozen source and reading its actual Cargo.lock SHA; no earlier file hashes or check results were rewritten. Both complete and partial prepared inventories then passed the new shared verifier. These are intermediate implementation snapshots, not proof of the final source or installed packages.

Additional cache input: `out/distribution/input-inventory/nfpm_2.47.0_Linux_x86_64.tar.gz` (SHA256 `0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783`) was copied from the parent's verified tool download and rehashed. Its receipt is `nfpm.json`.

After the offline-reuse check, the owned redundant `out/distribution/prepare-offline-reuse/` and partial `out/distribution/prepare-validation/inputs/` directories were removed. Retained for the final build: `out/distribution/input-inventory/`, `out/distribution/prepare-full-inputs/`, and `out/distribution/prepare-validation/` containing the reference manifest/source snapshot. The complete vendor location is `out/distribution/prepare-full-inputs/vendor`; its original config is `cargo-config.toml` beside it. Container builds must override the vendor directory to its container-visible path. The final prepare should use both `--cache out/distribution/input-inventory` and `--vendor-cache out/distribution/prepare-full-inputs`, and bind its new `prepared.json` to the final source snapshot.
