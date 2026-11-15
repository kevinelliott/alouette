# Third-party code

`matrix-sdk-sqlite/` is matrix-sdk-sqlite 0.19.1 from crates.io (Apache-2.0, by the Matrix.org
Foundation), unchanged except that its `Cargo.toml` asks for rusqlite 0.42 instead of 0.40.
rusqlite 0.40 bundles an SQLite affected by RUSTSEC-2026-0459. The workspace uses it through
`[patch.crates-io]`; remove both once a matrix-sdk release depends on a fixed rusqlite.
