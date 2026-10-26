# patched `h2` for `impit`

This is a patched version of `h2` for the `impit` project.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Crates.io](https://img.shields.io/crates/v/h2.svg)](https://crates.io/crates/h2)
[![Documentation](https://docs.rs/h2/badge.svg)][dox]

More information about this crate can be found in the [crate documentation][dox].

[dox]: https://docs.rs/h2

## impit: SETTINGS knobs

Two environment variables change the first SETTINGS frame a client connection sends, so impit can send the same
settings as a real browser:

* `IMPIT_H2_OMIT_SETTINGS`: comma-separated setting ids to leave out of that frame. Only `2` (ENABLE_PUSH), `5`
  (MAX_FRAME_SIZE) and `6` (MAX_HEADER_LIST_SIZE) are honoured; other ids are ignored.
* `IMPIT_H2_NO_RFC7540_PRIORITIES`: `0` or `1` adds setting 9 (SETTINGS_NO_RFC7540_PRIORITIES, RFC 9218) with that
  value; anything else is ignored.

Only that frame changes: h2 keeps enforcing the limits set on the client `Builder`. Unset or empty, the variables
change nothing.

One thing follows from the frame: with `2` left out, the server may push. h2 still has push disabled, so it refuses
each pushed stream with RST_STREAM REFUSED_STREAM, and the connection and the request carry on (with ENABLE_PUSH sent,
a push is still a connection error). Each refusal counts toward the connection's limit on local resets
(`max_local_error_reset_streams`, 1024 by default), as upstream h2 counts its own refusals since hyperium/h2#966.
Beyond that limit, the connection ends with GOAWAY ENHANCE_YOUR_CALM.

The variables are process-wide and are read when a client connection starts. The older `IMPIT_H2_PSEUDOHEADERS_ORDER`
(read in `src/frame/headers.rs` each time h2 encodes a header block) sets the pseudo-header order: a comma-separated
list naming all six, `:method,:scheme,:authority,:path,:protocol,:status` when unset; a missing or unknown name panics.

## Features

* Client and server HTTP/2 implementation.
* Implements the full HTTP/2 specification.
* Passes [h2spec](https://github.com/summerwind/h2spec).
* Focus on performance and correctness.
* Built on [Tokio](https://tokio.rs).

## Non goals

This crate is intended to only be an implementation of the HTTP/2
specification. It does not handle:

* Managing TCP connections
* HTTP 1.0 upgrade
* TLS
* Any feature not described by the HTTP/2 specification.

This crate is now used by [hyper](https://github.com/hyperium/hyper), which will provide all of these features.

## Usage

To use `h2`, first add this to your `Cargo.toml`:

```toml
[dependencies]
h2 = "0.4"
```

Next, add this to your crate:

```rust
extern crate h2;

use h2::server::Connection;

fn main() {
    // ...
}
```

## FAQ

**Is this an embedded Java SQL database engine?**

[No](https://www.h2database.com).
