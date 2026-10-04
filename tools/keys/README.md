# Signing keys

`oceans-dev.seed` is the **development** package signing key (ADR-0046):
the 32-byte Ed25519 seed, in hex. It signs the example packages that
`cargo xtask` builds into `build/packages/`, and every image this
repository builds trusts its public key, under the publisher name
`Oceans Examples` (the image's `trust.keys`).

It is public, since it is in this repository: **anyone can sign packages
with it**. It exists so that a fresh checkout can build, install and test
apps without any secret. A release image must not trust it. Release
images take their trust list and their publishers' keys from outside the
repository (to be defined with the release process, Phase 10).
