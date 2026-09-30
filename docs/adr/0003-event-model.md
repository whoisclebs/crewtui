# 3. Terminal input becomes typed events, and the app maps them to messages

Accepted.

## Context

A terminal gives a program bytes. The same key arrives as different sequences depending on the terminal and its modes, an Escape key is a prefix of most other sequences, a paste looks like typing, and a resize is a signal, not input. Apps should not see any of that.

## Options

1. Hand the app the bytes, or a thin wrapper, and let it parse.
2. Parse into a typed `Event` and pass it to a callback per kind of event.
3. Parse into a typed `Event` and have the app turn it into its own message, through one method.

## Decision

Option 3. `Parser` turns bytes into `Event`: keys with modifiers, mouse, bracketed paste as one string, focus in and out. The runtime adds `Event::Resize` from SIGWINCH. `App::event` maps an `Event` to `Option<Message>`, and everything after that is `update`.

A single reader thread waits on stdin, the signal pipe and a wake-up pipe together and sends into the channel the main loop blocks on. Effects send into the same channel. A lone Escape can't be told from the start of a sequence by the bytes alone, so the reader waits 50 ms for more input before it turns the Escape into a key.

Ctrl+C in raw mode is a key event and the app decides what it does. Signals from outside end the run instead; see the loop in `docs/architecture.md`.

`event` returns `Option`, so an app that doesn't care about a mouse or a focus change never sees it in `update`.

## Consequences

An app's `update` only ever sees its own messages, and a test can drive it by calling `update`, or `event` and then `update`, with no terminal.

Anything the parser doesn't know is dropped rather than shown as garbage. The Kitty keyboard protocol would add events and is not in (#27).
