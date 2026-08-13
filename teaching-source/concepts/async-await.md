# Async and await

## Core idea

Asynchronous work may take time without making the caller pretend the result is
already available. `await` pauses that task until its promise or future settles;
the UI can represent loading, success, and error meanwhile.

## Teaching order

Find the request start, the pending state, the completion path, and the stale
result guard. Explain what happens if the user changes the current change or
cancels before the work finishes.

## Common confusion

Async does not mean the work is automatically parallel, and `await` does not
mean the whole application freezes.

## Analogy

Ordering food and receiving a pager separates placing the order from collecting
the meal; the counter can serve other people while you wait.

## Level guidance

Beginner: trace loading to available or error.
Intermediate: discuss cancellation, request identity, event ordering, and
generation-based stale-result protection.
