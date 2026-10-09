# Hearfolio relay deployment

Before deployment, create `server/deploy/.env` with a randomly generated `RELAY_REGISTRATION_KEY` (32 random bytes or more) and permissions 0600. Production requires this key; missing configuration fails closed. Do not commit the file. Enter the key only for the first device in Hearfolio Settings. That device can invite others with a one-time QR/code: registration stays pending until the inviting device approves it (or its auto-pair setting approves it), then the relay issues an individual device token. Invited devices never need the shared key. The bootstrap key is not stored by the app and is not included in QR invitations.

Run from the repository root on the server:

```sh
docker compose -p hearfolio -f server/deploy/compose.yaml up -d --build
curl --fail http://127.0.0.1:8787/health
```

The HTTP relay listens only on the server's loopback interface. Add the location from `nginx.conf` to the existing HTTPS virtual host for `hearfolio.corextech.ru`. Keep the existing proxy and other sites intact, validate the proxy configuration before reloading it, and obtain a certificate valid for this hostname. If the server uses another reverse proxy, apply the same timeout and buffering settings there.

Validate the public endpoint without disabling certificate verification:

```sh
curl --fail https://hearfolio.corextech.ru/health
```

Expected response: `{"ok":true,"protocol":1}`. Then test pairing and transferring a recording between two clients through this public URL.

Device credentials and pairings persist in the `hearfolio_relay_state` Docker volume. Audio is forwarded in acknowledged 64 KiB blocks without saving it to disk. In-flight transfers and invitations expire; restarting the relay interrupts them. Back up the state volume privately; it contains authentication credentials. Do not include its contents in Git or logs. Do not run `docker compose down -v` during upgrades.

Upgrade using `up -d --build`; inspect `docker compose -p hearfolio -f server/deploy/compose.yaml logs --tail=50`. Stop using `down` while retaining the state volume. The current MVP limits the relay to 100 registered devices and 16 transfers; this deployment is intended for testing, not unrestricted public registration.

For this deployment, the repository snapshot is at `/home/www/hearfolio-relay` on `92.63.179.160`, accessed as `www`. Nginx uses the dedicated `hearfolio.corextech.ru` site; the matching Let’s Encrypt certificate renews with the existing Certbot timer and a domain-scoped Nginx reload hook. `hearfolio.nginx.conf` includes registration/request/connection limits and disables proxy disk buffering. HTTPS protects each client connection; the relay can read audio and transcripts in memory because end-to-end encryption is not implemented. Rotating the registration key blocks future registration with the old key; it does not revoke existing device tokens. Revocation requires removing the selected device from the state file while the relay is stopped, then restarting it; keep other device entries and pairings intact.

## Docker Hub publication

`.github/workflows/docker.yaml` follows [Dockerhub Publish](https://tasker.corex.studio/v/d/53). After a successful `Release` on `main`, it finds the `complete-vMAJOR.MINOR.PATCH` artifact belonging to that exact run, checks out the corresponding published tag, tests the relay and publishes `linux/amd64` and `linux/arm64` variants. A successful run without a new release is skipped. Tags are `MAJOR.MINOR.PATCH` and `latest`.

Create the `hearfolio` repository in Docker Hub and add GitHub repository secrets:

- `DOCKERHUB_USERNAME`: Docker Hub login.
- `DOCKERHUB_PASSWORD`: Docker Hub access token with permission to publish to this repository.

The image name defaults to `<DOCKERHUB_USERNAME>/hearfolio`. For an organization namespace, set the optional GitHub Actions variable `DOCKERHUB_IMAGE_NAME` to the complete `namespace/hearfolio` name. The registration key is a separate runtime secret on the server; it must never be added to the image or workflow.

The workflow must be merged into the default branch for `workflow_run` to trigger. Publishing the image does not automatically upgrade the running server. After a version is available, use Docker Compose >= 2.24.4 and replace the example image below with the actual namespace and published version:

```sh
export RELAY_IMAGE=your-namespace/hearfolio:0.3.2
docker compose -p hearfolio -f server/deploy/compose.yaml -f server/deploy/compose.registry.yaml pull
docker compose -p hearfolio -f server/deploy/compose.yaml -f server/deploy/compose.registry.yaml up -d --no-build
curl --fail https://hearfolio.corextech.ru/health
```

This retains the existing `hearfolio_relay_state` volume, runtime `.env`, loopback binding and container restrictions. Use a version tag for controlled upgrades and rollbacks; restarting interrupts active transfers. For a private Docker Hub repository, log in on the server first using a separate read-only token. Local builds continue to use `compose.yaml` without the registry overlay.

Invitation codes are single-use and valid for three minutes. A pending registration grants no device token. The join ticket remains native-only, can retrieve its approved token again if a response is lost, and expires after three minutes. Nginx limits `/join` with the registration rate limit. Rejecting a request grants no credentials. Address, device name and administrator key are configured in Settings; Devices contains invitations and paired devices.
