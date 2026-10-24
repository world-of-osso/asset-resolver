# Installed keyring discovery

Installed product/build selection remains authoritative in `.product.db`.
`.build.info` contributes keyring keys only from a row matching both the
selected product and build key. An absent matching row or empty `KeyRing` means
no installed keyring contribution; another product/build is never borrowed.
Unreadable or malformed referenced keyring data still errors, as do unavailable
keys required for decryption.

Run `cargo test --lib installed_build_tests` for concrete temporary-file fixtures
covering absent beta rows, wrong builds, empty and populated matching keyrings,
and referenced-keyring errors.
