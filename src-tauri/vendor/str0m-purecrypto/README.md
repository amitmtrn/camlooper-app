# str0m-purecrypto (vendored)

A vendored copy of [`str0m-rust-crypto`](https://crates.io/crates/str0m-rust-crypto) **0.7.0**
by Martin Algesten (<https://github.com/algesten/str0m>), dual licensed MIT OR Apache-2.0.
CamLooper uses it under the MIT license; the license text is in [`LICENSE-MIT`](LICENSE-MIT).

## Why it is vendored

CamLooper's Windows build is cross-compiled from Linux with `cargo-xwin`, so no dependency
may need OpenSSL, aws-lc-sys, cmake or nasm. That is the real constraint — not "pure Rust".
str0m's own `rust-crypto` feature does not meet it today: `str0m-rust-crypto` 0.7 enables
`dimpl/rcgen`, and in dimpl 0.9 that feature implies `dimpl/aws-lc-rs` and
`rcgen/aws_lc_rs`, which pull in `aws-lc-sys` (cmake + nasm).

This copy uses `ring` for one thing, signing the self-signed certificate. `ring` is **not**
pure Rust either (C and assembly, built with `cc`; prebuilt objects for the Windows MSVC
targets), but it cross-compiles with `cargo-xwin` without cmake or nasm, and the app already
depends on it: it is the rustls crypto provider for the relay connection (`reqwest`,
`tokio-tungstenite`). So it adds no new native build requirement. Everything else here
(DTLS via dimpl's `rust-crypto` feature, SRTP, SHA, HMAC) is RustCrypto. If `ring` ever
becomes a problem (aarch64-pc-windows-msvc needs clang for it), the alternative is an rcgen
`SigningKey` backed by the `p256` crate, which is already a dependency.

This copy is identical to upstream except for one thing, marked `PATCH` in `src/dtls.rs`:
dimpl is used with feature `rust-crypto` only, and the self-signed DTLS certificate is generated
with `rcgen` + `ring` instead of dimpl's `rcgen` feature. It is installed into str0m with the
public `RtcConfig::set_crypto_provider(...)`, so str0m itself is unpatched
(`str0m = { default-features = false }`).

## Dropping it

Remove this directory and switch to `str0m = { features = ["rust-crypto"] }` once upstream
str0m/dimpl decouple certificate generation from aws-lc (check with
`cargo tree -e normal --target all | grep -E 'aws-lc|cmake|nasm'`).

## Checks

The generated certificates are covered by a test in the app crate
(`dtls_certificates_are_valid_der_with_minimal_positive_serials` in
`src-tauri/src/remote/stream/rtc.rs`): 300 certificates must parse as strict DER X.509 with a
positive, minimally encoded, unique serial. (The 16 random serial bytes can start with
`0x00`; yasna strips leading zero bytes when it writes the INTEGER, so the encoding stays
minimal.)

---

Upstream README follows.

# str0m-rust-crypto

Pure Rust crypto backend for [str0m](https://github.com/algesten/str0m).
