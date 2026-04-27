# ADR-0001: Core logic calls log module directly

## Status

Accepted

## Context

xtask modules have core logic (e.g. `validate()` returning
`Vec<ValidationError>`) and orchestration functions (e.g. `run()`) that
tie core logic to CLI output. The question is where logging/presentation
side effects belong.

Three options were considered:

1. **Pure core, CLI renders**: Core functions return all data, CLI layer
   handles all output. Clean separation but forces every core function to
   surface all intermediate state (progress, warnings) as return values.
   Gets awkward when logging is needed mid-operation without a natural
   return point.

2. **Core calls `log::*` directly**: Orchestration functions in domain
   modules call `log::ok()`, `log::file_error()`, etc. Simple, works.
   Core has a side-effect dependency on the `log` module.

3. **Logging queue / callback**: Core pushes events to a subscriber. CLI
   consumes them. Clean separation but significant ceremony for a
   development tool.

## Decision

Option 2: core orchestration functions call `log::*` directly.

Pure functions (`validate()`, `split_frontmatter()`) remain pure and
return data. Orchestration functions (`run()`) are allowed to call
`log::*` for user-facing output.

## Rationale

- xtask is a development tool with exactly one consumer (the CLI). The
  purity of option 1 matters when multiple consumers exist; here there is
  one.
- Option 3 is designing for a hypothetical. If testability of log output
  ever matters, we can inject a writer or swap to callbacks then.
- The `log` module centralises styling so the presentation is consistent
  even though domain modules trigger it.

## Conventions

- Pure functions (parsing, validation, computation) return data. No `log`
  calls.
- Orchestration functions (`run()`) may call `log::*` to report progress,
  results, and errors.
- All user-facing output goes through `log::*`, never raw
  `println!`/`eprintln!`.

## CI compatibility

The `console` crate auto-detects non-TTY environments and disables colors
and styling. No `--no-color` flag or CI-specific codepath is needed --
`log::*` produces clean plain text when piped or running in CI. If
interactive prompts are ever added, they must check for a TTY or respect
a `--non-interactive` flag.
