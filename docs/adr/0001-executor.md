# 1. Effects run on a small thread pool

Accepted.

## Context

An app needs to stream LLM tokens, spawn processes, read files, wait on sockets and run timers, and `update` has to be able to ask for that. The core can't depend on tokio: a plain app has to work without any async runtime, and an app that does use one has to be able to feed it messages.

## Options

1. A worker pool in the core. `Cmd::perform` runs a blocking closure and posts the result back as a message.
2. A `Sender` handle any thread or runtime can use to inject messages.
3. An optional `tokio` feature with `Cmd::future`.

## Decision

Options 1 and 2, in the core, with no dependency. Option 3 is not done. A `tokio` app already has what it needs with 2: it spawns its own tasks and sends their results through a `Sender`, which is `Send` and `Clone` and needs no runtime to exist.

The details that matter:

- One channel carries everything the loop waits on: terminal events, signals and messages from effects. The loop blocks on it, so a message from a worker wakes it at once and nothing polls on a timer.
- The pool starts threads on demand up to eight, queues beyond that, and lets idle threads exit after five seconds. A job that blocks for a long time holds one of the eight slots. `Cmd::spawn` takes a slot too, which is what keeps thread count bounded.
- Timers live on one thread that sleeps until the earliest deadline. A repeating tick is `Cmd::after` returned again by the `update` that handled the last one, so there is nothing to cancel and no timer state in the app.
- Cancellation has two parts. When the program ends, timers that haven't fired and jobs that haven't started are dropped. A worker that is already running finds out through `Sender::send`, which returns `Closed` once the receiver is gone; a streaming worker stops when its send fails.
- A job that panics loses its result but not its worker thread.

## Consequences

Effects never touch state. They return messages, so `update` is still the only place state changes and the frame is still a function of it.

A blocking job can't be interrupted from outside. The program doesn't wait for one either: it quits and the worker's next `send` fails. Work that can't check for that, such as a single long blocking call, keeps running until it returns.

The limit of eight is a constant, not a setting. If a real app needs a different number it can become one.
