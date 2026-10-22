# Rust style

Ferese follows the [Rust Style Guide](https://google.github.io/styleguide/rustguide.html)
with the stable toolchain pinned in `rust-toolchain.toml`. Rules marked for Chromium
or Fuchsia belong to those projects, so they do not change Ferese's Cargo layout,
async runtimes, threading or test framework.

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
