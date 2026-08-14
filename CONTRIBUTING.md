# Contributing to Codex Mentor

Thanks for considering a contribution.

Codex Mentor aims to stay small, understandable, and focused on helping people review and understand AI-assisted code changes.

## Before you start

For a bug or small improvement, open an issue or submit a focused pull request.

For a larger architectural change, open an issue first so the scope can be discussed before significant implementation work begins.

## Development setup

Install dependencies:

```bash
npm install
```

Useful checks:

```bash
npm test
npm run lint
npm run build
```

If you have a working Rust/Tauri toolchain:

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

Run the desktop app with:

```bash
npm run tauri dev
```

## Contribution guidelines

Please keep changes:

- focused on one clear problem;
- easy to review;
- consistent with the existing architecture;
- free from unrelated refactors;
- readable without unnecessary abstraction;
- conservative about AI token/context usage.

Prefer removing redundancy over hiding reasoning in compressed code.

## AI-assisted contributions

AI-assisted development is welcome.

The contributor remains responsible for:

- understanding the submitted change;
- reviewing generated code;
- checking scope;
- running relevant tests;
- removing unrelated generated changes;
- accurately describing limitations.

Do not submit large generated patches that have not been reviewed.

## Tests

Add or update focused tests when behavior changes.

Before opening a pull request, run the checks relevant to your change and clearly state any checks you could not run.

## Pull requests

A good pull request should explain:

1. the problem;
2. the change;
3. why the change is needed;
4. tests or validation performed;
5. any remaining limitation.

Keep unrelated cleanup in a separate pull request.

## Project boundaries

Codex Mentor complements Codex. It is not intended to become an IDE, Codex replacement, autonomous coding agent, or general repository-analysis platform.

Changes should preserve the project's local-first, change-focused approach.
