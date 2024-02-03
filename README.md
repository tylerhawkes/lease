# Lease
This crate provides a `Pool` struct that allows taking `Lease`es and using them.
When a `Lease` is dropped it is automatically returned to the pool.

One nice thing about this api is that the lifetime of a `Lease` is not connected to the lifetime
of a `Pool` so they can be sent across threads.

There is also an `InitPool` that ensures that all new leases are created the same way

A `LockedPool` only allows calling functions that do not add or remove any `Lease`s

## Features
* `async`
  - Enables the [`Pool::get()`] function. Async brings a little bit of overhead to getting
    leases so it is behind a feature.
  - Enables the [`Pool::stream()`] function that allows getting a stream of leases as they become available