# Codex Mentor

**Understand the code AI writes.**

Codex Mentor is an open-source desktop companion for AI-assisted coding. It watches code changes, freezes the relevant before/after context, and helps developers review, understand, and learn from changes produced during a Codex workflow.

> Codex writes the code; Codex Mentor helps people understand the code.

## Why

AI coding agents can produce changes faster than people can review and understand them.

Codex Mentor focuses on the human side of that workflow:

- What changed?
- Why did it change?
- Which files deserve attention?
- How does the changed code work?
- Which programming concepts are involved?
- What should I learn from this change?

## Features

- Local project and change monitoring
- Frozen before/after file context
- Change statistics and review priority
- Change Analysis
- Engineer Mode
- Ask Mentor for questions about the current change
- Beginner Teaching Mode
- Intermediate Teaching Mode
- Teaching Source
- Learning Memory for tracked programming concepts

All current features are free and open source.

## Design principles

Codex Mentor is intentionally:

- local-first;
- change-focused;
- human-readable;
- small in architectural scope;
- conservative about unnecessary AI context;
- designed to complement Codex rather than replace it.

Most deterministic work stays local. AI is reserved for tasks where reasoning is useful, such as completed change analysis, teaching, and Ask Mentor.

## Project status

Codex Mentor is an early-stage open-source project.

The core implementation is complete and the frontend test/build/lint checks pass. Windows Rust/Tauri runtime validation is still in progress.

Expect changes while the first public release is prepared.

## Tech stack

- Tauri 2
- Rust
- React
- TypeScript
- Vite
- Vitest

## Development

### Prerequisites

You will need:

- Node.js and npm
- Rust and Cargo
- Tauri's Windows development prerequisites
- Codex CLI for Codex-backed Mentor workflows

### Install

```bash
npm install
```

### Run frontend tests

```bash
npm test
```

### Lint

```bash
npm run lint
```

### Build frontend

```bash
npm run build
```

### Run the Tauri app

```bash
npm run tauri dev
```

### Rust checks

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

## How it works

```text
Codex modifies a project
        ↓
Codex Mentor observes the change
        ↓
Changed files update locally
        ↓
Change completes
        ↓
Before / after context is frozen
        ↓
Change Analysis
        ↓
Engineer / Teaching modes
        ↓
Ask Mentor
        ↓
Learning Memory
```

Ask Mentor is designed to answer from the frozen current-change context rather than unnecessarily scanning the whole repository.

## Scope

Codex Mentor is not intended to become:

- an IDE;
- a replacement for Codex;
- an autonomous coding agent;
- a general-purpose repository analysis platform.

The focus is understanding and reviewing AI-assisted code changes.

## Contributing

Contributions, bug reports, and focused improvement proposals are welcome.

Please read [CONTRIBUTING.md](CONTRIBUTING.md) before submitting a pull request.

For security issues, see [SECURITY.md](SECURITY.md).

## License

Licensed under the [MIT License](LICENSE).
