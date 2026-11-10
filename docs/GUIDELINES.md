# Code guidelines

Rules for the Rust code in this workspace, each with its reason and a place in the code that follows it. The lints in `Cargo.toml` enforce some of them in `make ci`; review checks the rest.

Unless noted, the places are in `crates/moonmchn/src/daemon/machines.rs`.

## Ownership and drops

### Bind a guard to a named `_` variable

Write `let _claim = self.claim(..)?;`. A binding lives to the end of its scope, so the release covers every way out of the function. The leading `_` only silences the unused-variable warning.

`let _ = ..` is not a binding: it drops the value on that line. The `let_underscore_drop` lint rejects it for any type with a destructor.

- `Machines::start` and `Machines::stop` hold their claim this way.

### Bind inside a `move` closure what it must keep alive

A closure captures only what its body uses. To make a closure own a value for as long as it runs, bind the value in the body: `let _lock = lock;`. Without a use in the body, the clone stays with the caller's future, which can drop while the closure still runs.

- The blocking closures in `start`, `stop` and `stop_all` hold the daemon's lock until they end.

### Release in `Drop`

A value that holds a claim releases it in `Drop`, as `MutexGuard` does with its lock. `drop` runs on a normal return, an early return through `?`, and a panic unwinding, so a failed operation never leaves its claim behind.

- `InFlight` removes its name from the in-flight map.

### Tie a guard to its owner with a lifetime

A guard borrows the state it releases into. `InFlight<'a>` holds `&'a Mutex<Claims>`, and `claim(&self, ..) -> Result<InFlight<'_>>` borrows `self` for as long as the guard lives. The compiler rejects any code that keeps the guard after its `Machines` is gone.

### Consume a handle in the method that ends it

A method that ends a resource takes `self` by value. After the call, nothing can use the handle.

- `Machine::stop(self)` in `crates/moonmchn/src/machine.rs`.

### Discard a best-effort result with `drop(..)`

The lints deny `let _ = result;`, so a result that may fail without consequence goes to `drop(..)`, with a comment that says why its failure is fine.

- The marker rollback in `boot` and the stop announcement in `stop`.

## Locks

### Lock in a statement of its own

A temporary lives to the end of its statement. In an `if let` or `match` scrutinee, it lives to the end of the whole block, so a `MutexGuard` there holds the lock across everything inside. Copy what you need out of the map in a `let`, and branch on the copy:

```rust
let inflight = claims(&self.inflight).names.get(&name).copied();

if let Some(state) = inflight {
    // The lock is free here, before any I/O.
}
```

- `status` and `resolve`.

### Copy `Copy` values out of a map

Turn the `Option<&T>` that `get` returns into an owned value, so nothing borrows into the locked map: `.copied()` on the `Option`, or a `&` in the pattern.

- `.copied()` in `status`; `if let Some(&present) = inflight.names.get(name)` in `claim`.

### Change locked state in single steps

Every change under a lock is one step: an `insert`, a `remove`, a flag write. A panic then cannot leave the state half-updated, which makes it safe to take a poisoned lock's guard with `unwrap_or_else(PoisonError::into_inner)`. Code in `Drop` relies on this: a panic in `drop` while another panic unwinds aborts the process.

Lock briefly. A guard that lives for a whole operation keeps a reference to the `Mutex` and locks only when it changes the state.

- `claims`, and `InFlight`, which locks only in `claim` and in `drop`.

## Blocking work

### Run engine calls and file I/O on the blocking pool

The daemon's async threads never block. Engine calls and file I/O go through `blocking(..)`, which wraps `spawn_blocking`.

The closure must be `FnOnce() + Send + 'static`: it runs on another thread at any time, so it owns everything it uses. Write it as `move`, and clone beforehand whatever the caller still needs.

- `start` and `stop` run their whole operation in one `blocking` call.
- `list` spawns one query per machine to run them at once. Each `spawn_blocking` gets a block that clones the name for the closure and keeps the original for the `(name, handle)` pair.

### Flatten the join error at one place

`spawn_blocking(f).await` yields `Result<Result<T>, JoinError>`. The outer error means the closure panicked or was cancelled. `blocking` maps it into the crate's `Error` through `join_error` and applies `?`, so its result is the closure's own `Result<T>`. Await a single blocking call through `blocking` to get that conversion.

## Errors

### Roll back with `inspect_err`

`result.inspect_err(|e| ..)?` runs a side effect with a reference to the error, and `?` still returns the original error. Use it for cleanup on a failure that must reach the caller unchanged.

- `boot` removes the marker after a failed boot and reports the boot error.

### Chain fallible steps

Chain fallible calls with `?`, or with `and_then` when one `match` handles the error of every step: `E::Machine::attach(&name).and_then(Machine::stop)`.

## Idioms

### Pass a predicate to `str::starts_with`

`starts_with` takes a `fn(char) -> bool` as well as a `&str`, and returns `false` for an empty string. A name check needs no separate empty case.

- `validate_name`: `name.starts_with(is_name_start)`.

### Match a prefix list with `iter().any`

`iter()` on an array of `&str` yields `&&str`, which `starts_with` accepts through auto-deref. `any` stops at the first match.

- `REMOTE_SCHEMES` in `validate_mounts`.

### View a `String` as a path with `Path::new`

`Path::new(&string)` borrows the `String` as a `Path` without copying it. Use it for checks such as `is_absolute`.

- `validate_mounts`.

## Lints

### Take by reference what you only read

`clippy::pedantic` is denied across the workspace, and its `needless_pass_by_value` rejects a by-value argument the function only reads.

- `join_error` takes `&JoinError`.
