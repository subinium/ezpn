# Bundled terminal parser

Upstream: [doy/vt100-rust](https://github.com/doy/vt100-rust), crates.io
`vt100` 0.16.2, MIT (included LICENSE). This is a private module, not a new
public ezpn library API. The original crate source can be downloaded from
https://crates.io/api/v1/crates/vt100/0.16.2/download.
The package records upstream git revision
`eb66ffaf7d771c13303ef73b29f6f2a56fdacecf`.

Why bundled: regression tests reproduce tiny-grid arithmetic and clipped
wide-cell faults in 0.16.2, plus missing soft-wrap metadata. A Cargo path
patch is not used because publishing normalizes registry dependencies and
would lose the fixes. The actual parser sources ship inside the ezpn crate.

Mechanical changes: lib.rs becomes mod.rs; absolute crate paths point to
crate::vt100. Public upstream APIs unused by this binary are allowed as dead
code at this module boundary; product code and parser regression tests
remain subject to lint/test gates. Upstream opt-in Clippy lint groups are
replaced by the workspace's lint policy.

Behavioral patches and their regression tests are listed in the release
audit. Do not edit the global Cargo registry cache or remove failing tests.
The behavioral delta is restricted to grid.rs (mark wraps before a row can
scroll into history), row.rs (remove truncated wide-cell halves and bound
neighbor access), and screen.rs (oversized glyph and soft-wrap metadata).
The unused public Callbacks re-export is omitted; the trait and implementation
remain inside the private module. Formatting follows the workspace toolchain.
Re-evaluate upstream on each parser update and retain provenance/license.
