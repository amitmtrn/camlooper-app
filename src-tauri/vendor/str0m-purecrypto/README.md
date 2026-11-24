# str0m-purecrypto (vendored)

A vendored copy of [`str0m-rust-crypto`](https://crates.io/crates/str0m-rust-crypto) **0.7.0**
by Martin Algesten (<https://github.com/algesten/str0m>), dual licensed MIT OR Apache-2.0.
CamLooper uses it under the MIT license; the license text is in [`LICENSE-MIT`](LICENSE-MIT).

## Why it is vendored

CamLooper's Windows build is cross-compiled from Linux with `cargo-xwin`, so every dependency
must be pure Rust (no OpenSSL, cmake, nasm or aws-lc-sys). str0m's own `rust-crypto` feature
is *not* pure Rust today: `str0m-rust-crypto` 0.7 enables `dimpl/rcgen`, and in dimpl 0.9 that
feature implies `dimpl/aws-lc-rs` and `rcgen/aws_lc_rs`, which pull in `aws-lc-sys` (cmake +
nasm).

This copy is identical to upstream except for one thing, marked `PATCH` in `src/dtls.rs`:
dimpl is used with feature `rust-crypto` only, and the self-signed DTLS certificate is generated
with `rcgen` + `ring` instead of dimpl's `rcgen` feature. It is installed into str0m with the
public `RtcConfig::set_crypto_provider(...)`, so str0m itself is unpatched
(`str0m = { default-features = false }`).

## Dropping it

Remove this directory and switch to `str0m = { features = ["rust-crypto"] }` once upstream
str0m/dimpl decouple certificate generation from aws-lc (check with
`cargo tree -e normal --target all | grep -E 'aws-lc|cmake|nasm'`).

---

Upstream README follows.

# str0m-rust-crypto

Pure Rust crypto backend for [str0m](https://github.com/algesten/str0m).
