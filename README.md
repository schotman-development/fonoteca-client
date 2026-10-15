# Fonoteca Client

A music player for [Fonoteca](https://github.com/schotman-development/fonoteca)
and other OpenSubsonic servers, written in Rust with [Slint](https://slint.dev).

<a href="https://slint.dev"><img alt="Made with Slint" src="https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png" height="60"></a>

> [!NOTE]
> This is a bare scaffold. Nothing plays yet.

## Goals

- **Everything the server offers.** The whole OpenSubsonic API and every
  extension the server announces, not only the Subsonic subset most players
  stop at.
- **Browsing like Roon.** Every name on screen leads somewhere: from an album to
  its performers, from a performer to their other recordings, from a composer to
  their works, and back.
- **A component workbench.** Every element of the design system can be opened
  and tried on its own in a browser, like the Storybook beside Fonoteca's web
  UI.
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

The code is [MIT](LICENSE) licensed. Slint is used under the
[Slint Royalty-free Desktop, Mobile, and Web Applications License](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md),
which asks for the badge above.
