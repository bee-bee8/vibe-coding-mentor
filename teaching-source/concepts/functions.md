# Functions

## Core idea

A function is a named piece of work. It accepts inputs, performs a focused
responsibility, and may return a result. Calling it lets the same recipe run
with different values.

## Teaching order

Name the function's purpose, identify its inputs, trace its result, then show
which caller uses it. In Rust, visibility and the type signature also describe
who may call it and what shape of data crosses the boundary.

## Common confusion

Defining a function does not run it. A call is the moment execution enters its
body.

## Analogy

A function is a labeled recipe card: the label helps you find it, ingredients
are inputs, and the prepared dish is the result.

## Level guidance

Beginner: trace one call from input to result.
Intermediate: discuss responsibility boundaries, ownership of data, and why a
small function is easier to test.
