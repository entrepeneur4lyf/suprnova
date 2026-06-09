# Named HTTPS Dev URLs (`suprnova dev:tls`)

By default `suprnova serve` serves your backend on a raw
`http://127.0.0.1:8765`. That's fine for most development — but some
browser features only work over HTTPS on a named host:

- **Passkeys / WebAuthn** — require a secure context and a stable origin.
- **`Secure` cookies** and **`SameSite=None`** — only set over HTTPS.
- **Service workers** — only register over HTTPS (or `localhost`).
- **OAuth/OIDC redirect URIs** — providers often reject raw IP/port hosts.

[portless](https://portless.sh) gives every local app a stable
`https://<name>.localhost` URL behind a single TLS proxy on port 443.
`suprnova dev:tls` wires Suprnova to portless and — the part that's easy
to get wrong — trusts portless's local CA in **every browser certificate
store on your machine**, with no sudo on Linux.

> **Strictly opt-in.** portless is never required. `suprnova serve` works
> with no portless installed. You opt in when you scaffold
> (`suprnova new <name> --with-portless`) or by adding `portless.json`
> later. If you never run `dev:tls`, you never touch portless.

## Install portless

portless is a Node tool:

```bash
npm install -g portless
```

Then install its always-on 443 proxy once (this is a system-level,
sudo-required step that belongs to portless, not Suprnova):

```bash
portless service install
```

## Per project

You have two ways to opt a project in.

**Scaffold with the flag** — writes `portless.json` up front:

```bash
suprnova new myapp --frontend svelte --with-portless
```

That emits a `portless.json` at the project root:

```json
{
  "name": "myapp",
  "appPort": 8765
}
```

`appPort` is your backend's fixed `SERVER_PORT`. It tells portless the app
binds a known port (instead of portless assigning one via `$PORT`), so the
named URL routes straight to it.

**Add it to an existing project** — write that same `portless.json` by
hand (or run `portless alias myapp 8765`), using your `SERVER_PORT`.

Then, on **each machine** that will run the app, do the one-time trust +
route registration:

```bash
cd myapp
suprnova dev:tls
```

This:

1. Checks `portless` is on your PATH.
2. Resolves the name (`--name`, else `Cargo.toml` `[package].name`) and
   port (`--port`, else `SERVER_PORT`, else `8765`).
3. Registers the route `myapp.localhost → 127.0.0.1:8765` (skip with
   `--no-alias`).
4. Trusts portless's CA in your browsers' certificate stores.
5. Prints the next steps.

Flags:

| Flag | Effect |
|---|---|
| `--name <name>` | Override the URL name. Default: `Cargo.toml` package name. |
| `--port <port>` / `-p` | Override the routed port. Default: `SERVER_PORT`, else `8765`. |
| `--no-alias` | Only trust the CA; don't touch the portless route. |

## Run

```bash
suprnova serve
```

Open `https://myapp.localhost`.

The backend binds `8765` by default; the Vite dev server rides along on
`5765` over `http://localhost`. A page served from the HTTPS origin can
reference `http://localhost` assets because browsers treat `localhost` as
a secure context — it is **not** blocked as mixed content.

> **Hot Module Reload over HTTPS is best-effort.** Vite's HMR websocket
> connects back to the dev server; whether that succeeds cleanly over the
> HTTPS origin depends on your Vite/browser versions. If live updates
> stop working under `https://`, point Vite at an HTTPS dev-server origin
> via the `INERTIA_VITE_DEV_SERVER` environment variable. Page loads and
> the rest of the flow are unaffected.

## Multiple apps

portless owns 443 and multiplexes by subdomain. Register each app with
its own name and port:

```bash
suprnova dev:tls --name app-one --port 8765
suprnova dev:tls --name app-two --port 8766
```

Never bind 443 from an app directly — that's portless's job.

## Troubleshooting

**`ERR_CERT_AUTHORITY_INVALID` after running `dev:tls`.** Your browser
wasn't fully restarted. Browsers read their certificate store once at
launch; a tab reload is not enough. Type `chrome://restart` (or fully
quit and relaunch).

**`502 Bad Gateway`.** The proxy is up but your backend isn't. Run
`suprnova serve` in the project directory.

**`portless trust` says "A terminal is required to authenticate".**
That's portless's own command needing a real TTY for `sudo`.
`suprnova dev:tls` sidesteps it entirely on Linux: it installs the CA
straight into your browsers' NSS stores, which need no sudo.

**A Flatpak browser is still untrusted.** Flatpak browsers keep their NSS
database under `~/.var/app/<id>/.pki/nssdb`. `dev:tls` covers those —
re-run it and fully restart that browser.

**`certutil: command not found`.** Install NSS tools:

| Distro | Command |
|---|---|
| Debian/Ubuntu | `sudo apt install libnss3-tools` |
| Fedora/RHEL | `sudo dnf install nss-tools` |
| Arch | `sudo pacman -S nss` |

**`portless CA not found at ~/.portless/ca.pem`.** portless generates its
CA when the proxy first runs. Start it once
(`systemctl start portless`, or `portless proxy start`), then re-run
`suprnova dev:tls`.

## Platform notes

The browser-NSS path above is the Linux mechanism. On **macOS** and
**Windows**, browsers read the OS keychain / certificate store, so
`dev:tls` delegates CA trust to `portless trust`, which targets those
native stores.
