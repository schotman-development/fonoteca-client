# Fonoteca Client

A music player for [Fonoteca](https://github.com/schotman-development/fonoteca)
and other OpenSubsonic servers, written in Rust.

> [!NOTE]
> This is a bare scaffold. Nothing plays yet.

## Goals

- **Everything the server offers.** The whole OpenSubsonic API and every
  extension the server announces, not only the Subsonic subset most players
  stop at.
- **Browsing like Roon.** Every name on screen leads somewhere: from an album to
  its performers, from a performer to their other recordings, from a composer to
  their works, and back, the way Roon and Fonoteca's web UI browse.
- **A component workbench.** Every element of the design system can be opened
  and tried on its own, like the Storybook beside Fonoteca's web UI.
- **Three platforms:**
  - Android, for GrapheneOS: no Google Play Services
  - Linux, as an AppImage and a Nix package
  - macOS, unsigned

## Development

Install Rust with [rustup](https://rustup.rs); `rust-toolchain.toml` picks the
toolchain.

```sh
cargo run
```

## License

[MIT](LICENSE)
