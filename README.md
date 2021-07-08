# Lease
This crate provides a `Pool` struct that allows taking `Lease`es and using them.
When a `Lease` is dropped it is automatically returned to the pool.

One nice thing about this api is that the lifetime of a `Lease` is not connected to the lifetime
of a `Pool` so they can be sent across threads.

## Features
* `async`
  - Enables the `Pool::get_async()` function. Async brings a little bit of overhead to getting
    leases so it can be disabled. On by default.