# Settings parser mutation audit

This audit records `src/config.rs` at [a1d35daaeea8eb220d69fcd6ee193f925ed621b6](https://github.com/DegradingAnt/enshrouded-headroom/tree/a1d35daaeea8eb220d69fcd6ee193f925ed621b6), after HDR paper white, HDR peak and the settings property tests merged. The earlier [W16 audit](https://github.com/DegradingAnt/enshrouded-headroom/pull/20) covered other source versions. Its whole-project totals are not new measurements of this commit.

A mutant is a deliberate source change. “Caught” means the changed code compiled and a test failed. “Unviable” means the changed code could not build; it is not counted as caught.

## Results

Measured on Windows x64 with Rust 1.99.1 and cargo-mutants 27.1.0.

| Run | Source commit | Generated mutants | Caught | Missed | Unviable | Timed out |
|---|---|---:|---:|---:|---:|---:|
| Initial full sweep | `a1d35da` | 66 | 61 | 0 | 5 | 0 |
| Validated before, after the focused retry below | `a1d35da` | 66 | 62 | 0 | 4 | 0 |
| Final full sweep | `a1d35da` | 66 | 62 | 0 | 4 | 0 |

The initial sweep's fifth unviable result was a linker output-file error (`LNK1104`) for a `Reader::report_unknown` comparison. This did not establish a source-level compile failure. A focused retry of both equality mutations in that function compiled and caught both. The validated row replaces only their outcomes by exact mutant name; it does not add them again to the 66-mutant population. The original 61-caught/5-unviable summary is preserved.

The four source-level unviable mutations replace these return values with `Default::default()`:

| Return function | Compile failure |
|---|---|
| `From<LogLevel> for log::LevelFilter::from` | `log::LevelFilter` has no `Default` implementation. |
| `Reader::choice` | Its generic `T` has no `Default` bound. |
| `Reader::texture_budget` | `TextureBudget` has no `Default` implementation. |
| `Reader::paper_white_nits` | `PaperWhiteNits` has no `Default` implementation. |

There were no surviving compiled mutants, so no parser or test change was needed. Existing settings tests and the bounded property tests provide the failures. The production file and all existing tests remain unchanged.

## Reproduce

Use the pinned tool, then run the same per-file policy as W16. Choose a separate output directory for each full sweep so the results remain available.

```powershell
cargo mutants --no-config --file src/config.rs --timeout 30 --build-timeout 180 --cargo-arg=--lib --jobs 4 --output audit/config-before
cargo mutants --no-config --file src/config.rs --timeout 30 --build-timeout 180 --cargo-arg=--lib --jobs 4 --output audit/config-after
```

The focused retry used:

```powershell
cargo mutants --no-config --file src/config.rs --re 'replace == with != in Reader::report_unknown$' --timeout 30 --build-timeout 180 --cargo-arg=--lib --jobs 4 --output audit/config-retry
```

Both full sweeps use the same generated inventory. There is no sampling, new exclusion, iterate-skipping or in-place mutation. Temporary copies keep deliberate mutations away from the source checkout. Raw tool inventories, outcomes, diffs and build/test logs support the counts.

## Scope

This measures the generated mutation population for the settings parser at the pinned commit. It does not rerun the earlier scanner, governor or native-module audits. The tool's existing skips for test-only code, unsafe functions, constructors, Default implementations and capacity-only calls still apply. No exclusion was added to hide a survivor. Default-ignored executable and desktop tests stay ignored.
