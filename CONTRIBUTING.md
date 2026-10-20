# Contributing

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

## Reporting

- **Bugs and ideas:** open an [issue](https://github.com/schotman-development/fonoteca-client/issues/new/choose).
  For a bug, say which server and version you use and how to make it happen.
- **Questions:** ask in [Discussions](https://github.com/schotman-development/fonoteca-client/discussions).
- **Security problems:** never in a public issue. See [SECURITY.md](SECURITY.md).

## Building

See [Development](README.md#development) in the README. In short:

```sh
cargo run                    # the app
cargo run --bin workbench    # the component workbench
podman compose up -d         # both in a browser, on ports 8090 and 8091
```

A server to try it against: [Navidrome](https://www.navidrome.org) runs in one
container, or use [Fonoteca](https://github.com/schotman-development/fonoteca).

## Sending a change

1. For anything larger than a fix, open an issue first, so we agree on the
   shape before you build it.
2. Keep the change to one thing. Don't reformat or tidy code it doesn't touch.
3. Before you push, run:

   ```sh
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

4. For a change to the UI, look at it at a desktop and a phone width, in light
   and dark mode, and add screenshots to the pull request.

### Guidelines

- **The spec leads.** The app follows the [OpenSubsonic API](https://opensubsonic.netlify.app)
  and works with any server that speaks it. A server's own extensions are
  optional: check that the server announces them and keep the app working
  without them.
- **Secrets stay in the secret store.** Passwords, API keys and tokens go to the
  server they are for and to the system's secret store, never to a file, a log
  or another service. The browser preview, which has no secret store, is the
  one exception.
- **No telemetry.** The app talks only to the servers the user signs in to,
  and to ListenBrainz when the user gives it a token.
- **The UI stays responsive.** Show the data first and the images after, and
  never blank what is already on screen while loading.

### Commits

Commits follow [Conventional Commits](https://www.conventionalcommits.org), with
a short subject in the imperative and a body that says why:

```
feat: browse the server's music folders
fix: keep the pages and forms above a phone's keyboard
feat(opensubsonic): add getScanStatus and startScan
```

Use the crate as the scope when a change stays inside one of the libraries.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
