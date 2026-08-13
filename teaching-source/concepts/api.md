# APIs

## Core idea

An API is a documented boundary between callers and an implementation. A
caller provides an agreed request shape and receives an agreed response shape
without needing the implementation's private steps.

## Teaching order

Identify the command or function, its inputs, its result or event, and the
state transition it represents. Then connect the boundary to its concrete
caller and test.

## Common confusion

An API is not necessarily a network service. A local Tauri command and a Rust
function can both be APIs.

## Analogy

An API is a counter at a library: you ask using the library's rules and receive
the requested result while the back room stays hidden.

## Level guidance

Beginner: explain the request and response as a conversation.
Intermediate: focus on contracts, serialization, failure behaviour, and
coupling between layers.
