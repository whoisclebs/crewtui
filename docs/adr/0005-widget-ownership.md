# 5. Widgets are values built each frame, and state belongs to the app

Accepted.

## Context

`view` takes `&self`. It can't change the app, and it runs after every message. Widgets need data (text to show), and some need state that outlives a frame (which item is selected, how far a list has scrolled).

## Options

1. Widgets are objects the app creates once and keeps, and `view` calls methods on them.
2. Widgets are cheap values built inside `view` from the app's data and dropped after drawing. State that must persist is a separate value the app owns.
3. A framework-owned tree of components with their own state and identity.

## Decision

Option 2. `Widget::render(self, area, buf)` consumes a value, so building a `Paragraph` per frame costs a few fields and borrows the text. `StatefulWidget::render(self, area, buf, &State)` takes the state by shared reference, so it can be drawn from `view`.

What the app decides (the selected index, the input text, the scroll position) is plain data in the state and changes in `update`. What only drawing can know, such as how far a `List` scrolled to keep the selection visible or how many rows a `History` entry wraps into at this width, is derived during render and kept in a `Cell` or `RefCell` inside the state, so it is still there next frame.

Option 3 is the browser model this crate stays away from: identity, reconciliation and lifecycle for something that is redrawn from state anyway.

## Consequences

Two draws of the same state give the same frame, apart from caches that only make later frames cheaper.

The state types have interior mutability, so they are not `Sync`. A state shared between threads has to be cloned or wrapped by the app.

Widgets that are expensive to build must keep the expensive part in their state, as `History` does with its measured rows. A widget built from scratch each frame from a large input would pay for it each frame.
