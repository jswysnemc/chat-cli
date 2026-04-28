# Release Workflow

This project releases from version tags.

## Local Release Checklist

1. Update `Cargo.toml` to the next version and refresh `Cargo.lock`.
2. Update `CHANGELOG.md` with user-facing changes.
3. Run local checks:

   ```bash
   cargo fmt --check
   cargo test
   cargo build --release
   ```

4. Commit the version and release maintenance changes.
5. Create an annotated tag:

   ```bash
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   ```

6. Push `main` and the tag:

   ```bash
   git push origin main
   git push origin vX.Y.Z
   ```

7. Check the `Release` workflow. The workflow publishes GitHub release assets first, then updates AUR package `chat-cli-bin` if `AUR_SSH_PRIVATE_KEY` is configured.

## AUR Maintenance

Tagged releases run `scripts/update-aur-bin.sh` after GitHub release assets are available. The script downloads the Linux x86_64 release archive, calculates its SHA-256 checksum, and writes `PKGBUILD` plus `.SRCINFO` into the AUR checkout.

The AUR job is skipped with a workflow notice when `AUR_SSH_PRIVATE_KEY` is absent.
