# Rust style

Ferese uses the stable toolchain pinned in `rust-toolchain.toml`. The rules below
cover module organization, API design, ownership, safety and formatting.

Keep each change focused enough to review and test on its own. Formatting, module
moves and behavioral fixes should have separate commits, and a refactor should
retain the assertions that established the existing behavior.

## Modules and imports

Keep library roots focused on module declarations and re-exports, and put substantial
implementations in modules named for the types they own. Application entry points
can contain the wiring needed to start their services; splitting that wiring should
make ownership clearer rather than add forwarding layers.

Import types by name and qualify functions through their module unless their names
already identify their purpose unambiguously. Each import should draw from one
module. Avoid glob imports, with the guide's exceptions for inline tests, local enum
matches and re-exports. The Wayland scanner's generated namespaces also need glob
imports, and GStreamer's prelude supplies its extension traits.

Import traits inside the functions that use them unless their methods are used
throughout the file. Use inherent implementations for a type's own methods; define
traits for generic constraints, dynamic dispatch or extension methods.

## APIs and ownership

Borrow the data a function needs: use `&str` and slices instead of references to
owned containers, and `Option<&T>` instead of `&Option<T>`. Clone when the caller
needs independent ownership, including messages that outlive the current callback.
Shared worker state and UI resources can use `Arc` when their lifetimes require it.

Name fallible constructors `try_new`, use standard traits when their contracts fit,
and mark pure return values with `#[must_use]`. Prefer named fields when tuple
positions would conceal meaning. Document constraints, failure conditions and
ownership that callers cannot infer from the signature, without repeating obvious
method names in comments.

Give type lifetimes names that describe the borrowed resource, such as `'socket`,
`'window` or `'content`. Short lifetimes remain appropriate for simple relationships
between a function's inputs and outputs.

Build iterator chains in a named binding before a `for` loop. Retain borrowed
temporary values and lock guards in explicit bindings, preserving their original
scope and drop order. Keep simple iteration direct, and use ordinary loops when a
combinator chain would obscure control flow.

Ferese's workspace crates evolve together. Their shared enums can remain exhaustive
so adding a state makes every affected consumer visible to the compiler. Apply
`#[non_exhaustive]` when an API crosses an independently maintained boundary and its
consumers cannot be updated together.

## Safety and errors

Use `Result` when callers need to distinguish recoverable failures and `Option` when
absence explains the outcome. Match a checked value rather than checking and then
unwrapping it. If an invariant requires a panic, use `expect` to explain it.

Keep unsafe operations at their system or library boundary. Every unsafe block needs
a `SAFETY:` comment explaining its actual pointer bounds, ownership, lifetime or ABI
requirements, and unsafe functions must document what their callers guarantee.
Explain why the operation requires unsafe code, and use initialized fields or safe
library operations when they provide the same behavior. Document public panic cases
caused by invalid inputs, alongside recoverable error conditions.
Never recover the contents of a poisoned mutex. A disposable cache may be skipped;
state needed for correctness must fail or stop being used.

## Checks

Stable rustfmt uses a 100-column target. Keep imports grouped by module during
review, since stable rustfmt does not yet enforce that setting. All workspace
members inherit the shared Rust and Clippy lints from the root manifest.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --no-fail-fast
python3 -m unittest discover -s scripts/tests -v
```

Use the [native test guides](../development.md#test-guides) for changes to rendering,
input, surface lifetimes or process supervision. Headless checks complement native
and hardware testing; they do not establish frame pacing or direct-session stability.
