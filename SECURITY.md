# Security policy

## Supported versions

Only the latest [edge build](https://github.com/schotman-development/fonoteca-client/releases/tag/edge)
and the `main` branch get fixes. There are no stable releases yet.

## Reporting a vulnerability

Please report it privately through
[GitHub's private vulnerability reporting](https://github.com/schotman-development/fonoteca-client/security/advisories/new),
not in a public issue or discussion.

Say what an attacker can do, on which platform, and how to reproduce it. Once
a fix is out, the advisory is published, with credit to you if you want it.

## Scope

Of most interest are ways the app could leak a password, API key or token:
to a file, a log, the clipboard, or any address other than the server it was
entered for. Also in scope: accepting an invalid TLS certificate, and anything
a malicious server could do to the device through the app.

Problems in a server, such as Fonoteca or Navidrome, belong in that server's
own repository.
