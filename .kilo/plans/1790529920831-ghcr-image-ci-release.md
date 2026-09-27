# CI: publish Naslos images to GHCR + a release that stays compatible with Naslos-Installer

Plan ID: `1790529920831`
Status: implementation-ready (decisions locked with the user 2026-09-27)
Repo: `AessemOps/Naslos-Linux` — branch `ci/ghcr-image-release`, PR against `master`.
Out of scope: the VM path (`scripts/deploy-vm.sh`, `charts/naslos/values-vm.yaml`, the private registry `192.168.1.2:30095`) is unchanged.

## 1. Goal

Add GitHub Actions CI to Naslos-Linux that:

1. **On every PR and push to `master`** — run the full gate sweep (`scripts/audit.sh`) and build every image (no push), so a broken Go/UI/chart/Dockerfile change fails the PR.
2. **On a strict-semver `vX.Y.Z` tag** — build + push all 8 images to `ghcr.io/aessemops/naslos-*` (linux/amd64, public), then build the install pack with **every image pinned by digest**, attach it to the GitHub release, and dispatch `Naslos-Installer` (the existing `naslos-release` `repository_dispatch`).

**Cross-repo compatibility is the hard constraint.** `Naslos-Installer` resolves the newest strict-semver `vX.Y.Z` tag (`^v[0-9]+\.[0-9]+\.[0-9]+$`), downloads `naslos-install-pack-<version>.tar.gz` from that release, verifies the checksum, and embeds it; its `Makefile` derives `ExpectedTalosVersion`/`ExpectedSchematicID` from the pack. Therefore one tag must produce **both** the images and the digest-pinned pack, atomically. Never push a throwaway strict-semver tag (the installer would treat it as "newest").

## 2. Decisions locked (user, 2026-09-27)

- **Public GHCR packages.** Images carry no secrets. The node the installer provisions has **no `imagePullSecret`** (none exists in the chart or contract), so packages must be public to pull anonymously. Git repo stays private; package visibility is independent. A one-time manual "make package public" step is documented (§10); optional automation is noted.
- **linux/amd64 only.** Matches the installer contract (amd64-only, spec §1.4) and the ZFS ISO. arm64 is future work.
- **Publish only on `vX.Y.Z` tags.** PRs/`main` build images without pushing. No moving `latest`/`edge` tag namespace (avoids the `IfNotPresent` mutable-tag trap, NAS-022 / VER-2).
- **Full audit gate.** `scripts/audit.sh` (Go vet/test -race for both modules, svelte-check, helm lint on both profiles, install-pack checksum check, the AUDIT-* chart assertions, gitleaks history, govulncheck, gosec) runs in CI with the security tools installed, plus a Docker build of every image.

## 3. Current state / gaps

- No `ci.yml`; `scripts/audit.sh` is run manually (its own header says "There is no CI workflow wired to it for now").
- Only `.github/workflows/install-pack.yml`: tag → `make install-pack` → attach to release → dispatch installer. It does **not** build/publish images.
- Images are built/pushed by hand to the private registry (`make images`/`push-images`, `deploy-vm.sh`). Image tags are `0.1.0-rNN`.
- `charts/naslos/values-installer.yaml` points at `ghcr.io/aessemops/naslos-*` with **tag placeholders and no digests** — the NAS-022 residual called out in `AI_Handoff.md`.
- The chart already supports digests: `api|ui|agent|shares.smb|shares.nfs|terminal .image.digest` (renders `repository@sha256:…` when set) and `openldap.image` is a full reference string. `documented`/VER-2.
- `INSTALLER_DISPATCH_TOKEN` is not yet configured in the repo (installer handoff); the dispatch step is skipped when absent.

## 4. Image inventory (8)

| Component | GHCR repository | Build context | Dockerfile | Notes |
| --- | --- | --- | --- | --- |
| api | `ghcr.io/aessemops/naslos-api` | `.` | `api/Dockerfile` | |
| agent | `ghcr.io/aessemops/naslos-agent` | `.` | `agent/Dockerfile` | |
| ui | `ghcr.io/aessemops/naslos-ui` | `.` | `ui/Dockerfile` | Node 20 build |
| samba | `ghcr.io/aessemops/naslos-samba` | `samba/image` | `samba/image/Dockerfile` | |
| nfs | `ghcr.io/aessemops/naslos-nfs` | `nfs/image` | `nfs/image/Dockerfile` | |
| terminal | `ghcr.io/aessemops/naslos-terminal` | `terminal/image` | `terminal/image/Dockerfile` | |
| openldap | `ghcr.io/aessemops/naslos-openldap` | `openldap/image` | `openldap/image/Dockerfile` | |
| buddy-receiver | `ghcr.io/aessemops/naslos-buddy-receiver` | `.` | `api/Dockerfile.receiver` | `--build-arg VERSION=<ver>`; standalone, **not** in the chart/pack |

The first 7 are the chart images the pack must pin; `buddy-receiver` is published for the standalone Docker use case only.

## 5. Workflows

### 5.1 `.github/workflows/ci.yml` (new)

Triggers: `pull_request`, `push: branches: [master]`, `workflow_dispatch`.
`permissions: contents: read`.

- **job `audit`** (ubuntu-latest):
  - `actions/checkout` (fetch-depth 0 — gitleaks history).
  - `actions/setup-go` with `go-version-file: api/go.mod` (1.26.6) and cache.
  - `actions/setup-node` v20 with npm cache; `cd ui && npm install --no-audit --no-fund` (the UI has no committed lock usable on CI; `ui/Dockerfile` already resolves in-image for the same reason).
  - Install `helm`, `python3` + `pyyaml` (`python3 -m pip install pyyaml` — `scripts/audit.sh` and `deploy-vm.sh` parse YAML with PyYAML).
  - Install `govulncheck` (`golang.org/x/vuln/cmd/govulncheck@latest`) and `gosec` (`github.com/securego/gosec/v2/cmd/gosec@latest`) so the audit's security gates actually run instead of skipping.
  - `bash scripts/audit.sh` (already runs `go test -race` unconditionally, `helm lint` both profiles, `render-installer-template.sh --check`, `check_install_pack`, the AUDIT-M3/M4/FR-APP-15/FR-DNS assertion scripts).
  - Add a new assertion inside `scripts/audit.sh` (§7.4): with `--set api.image.digest=sha256:<64 hex>`, `helm template` renders `naslos-api@sha256:…` — this pins the digest mechanism the pack relies on.
- **job `gitleaks`** (ubuntu-latest): `gitleaks/gitleaks-action` pinned by SHA, `GITLEAKS_LICENSE` only if needed for orgs (public action works without on a private repo via `GITHUB_TOKEN`). Kept separate from `audit` so a secret hit is a first-class check.
- **job `images-build`** (matrix over the 8 §4 entries): `docker/setup-buildx-action` (SHA-pinned) + `docker/build-push-action` with `push: false`, `platforms: linux/amd64`, `cache-from/to: type=gha`, `build-args: VERSION=ci` for buddy-receiver. Catches Dockerfile breakage without publishing.
- **job `pack`**: `make install-pack` and validate the tarball + `metadata.json` checksums (the build is also inside `audit.sh`; keep this job so a pack failure is named explicitly). No digests (tag placeholders) — this is the local/dev pack shape.

All `uses:` third-party actions must be pinned to immutable commit SHAs (repo convention; `install-pack.yml` already does this). The implementer resolves the current SHAs.

### 5.2 `.github/workflows/release.yml` (new; replaces `install-pack.yml`)

Triggers: `push: tags: ["v*"]`, and `workflow_dispatch` with inputs `version` (required, e.g. `0.1.0`) and `publish` (boolean, default `false`) for safe dry runs.
`permissions: contents: write`, `packages: write`.

- **job `guard`**:
  - Parse `${GITHUB_REF_NAME#v}` (or the `workflow_dispatch` `version`) and assert it is strict semver `X.Y.Z`.
  - Run `scripts/check-release-version.sh <version>` (§7.3): assert `charts/naslos/Chart.yaml` `version` **and** `appVersion`, and `ui/package.json` `version`, all equal `<version>`. Fail otherwise (prevents a `v0.2.0` tag shipping a `0.1.0` chart/pack). This is a deliberate new gate — the current tree has `0.1.0` everywhere, so bumping the three files becomes part of every release commit.
- **job `images`** (`needs: guard`, matrix over the 8, `fail-fast: false`):
  - `docker/setup-buildx-action` + `docker/login-action` (`registry: ghcr.io`, `username: ${{ github.actor }}`, `password: ${{ secrets.GITHUB_TOKEN }}`).
  - `docker/build-push-action` with `push: true`, `platforms: linux/amd64`, `provenance: true`, `sbom: true`, and tags:
    `<component>:${{ version }}` and `<component>:sha-${{ github.sha_short }}`.
  - Each cell writes its digest to `digest-<component>.txt` from `steps.build.outputs.digest` (`sha256:…`) and `actions/upload-artifact`.
- **job `pack`** (`needs: images`):
  - `actions/download-artifact` all `digest-*`; merge into `dist/image-digests.json` (map component → `ghcr.io/aessemops/naslos-<component>@sha256:…`).
  - Verify every digest resolves: `docker buildx imagetools inspect <ref>` for each (a copy/`--load` is unnecessary). This proves the pack points at pullable, immutable images.
  - `make install-pack PACK_VERSION=<version> IMAGE_DIGESTS_FILE=dist/image-digests.json` (arms the digest pinning, §6).
  - `helm template` the **pack's** chart with the pack's `values-installer.yaml` to prove it renders with digests (`--set openldap.bindPassword=lint-only` is no longer needed: the installer profile has chart-generated Secrets; keep the flag only if the render needs it).
  - Attach `dist/naslos-install-pack-*.tar.gz` + `.sha256` to the GitHub release (`softprops/action-gh-release`, SHA-pinned; `fail_on_unmatched_files: true`). For `workflow_dispatch` with `publish=false`, upload the pack as a workflow artifact and **skip** the release + dispatch.
  - Dispatch the installer: the existing `gh api repos/AessemOps/Naslos-Installer/dispatches -f event_type=naslos-release -f client_payload[tag]=… -f client_payload[version]=…` step, guarded on `INSTALLER_DISPATCH_TOKEN`, skipped when `publish=false`.
- Delete `.github/workflows/install-pack.yml` (its tag→pack→release→dispatch logic moves here; the dispatch **event type and payload are unchanged**, so the installer needs no change).

Sequencing inside one workflow is what makes the tag atomic: images land before the pack is built, and the pack never references an unpublished digest.

## 6. Digest pinning into the pack

The in-tree `charts/naslos/values-installer.yaml` stays the **template** (tag placeholders, `digest: ""`), so local `make install-pack` and `audit.sh` keep working. At release time, `scripts/build-install-pack.sh` rewrites the copy it puts in the pack:

- Reads `IMAGE_DIGESTS_FILE` (JSON: `{"api":"ghcr.io/…@sha256:…", …}`, or a flat `component=ref` lines file).
- For `api`, `agent`, `ui`, `shares.smb`, `shares.nfs`, `terminal`: set `.image.digest = "sha256:…"` (keep `.image.tag` for humans; the chart renders `repository@sha256:…`, and a digest wins over a tag — VER-2).
- For `openldap`: set `openldap.image` to the full `ghcr.io/aessemops/naslos-openldap@sha256:…` (it is a single reference string).
- **Fail closed**: when a digests file is supplied, require all **7** chart images; list any missing and exit non-zero. Local builds with no `IMAGE_DIGESTS_FILE` keep the tag placeholder (unchanged behavior).
- Additively extend `metadata.json` with `images: {component: "repo@sha256:…"}` for the 7 pinned images. The installer's `internal/installpack` ignores unknown JSON fields, and `formatVersion` stays `1`, so the pack remains compatible. (Optional but recommended — it lets the installer's recovery README record exact image digests.)

Because the engine applies the pack's `values-installer.yaml`, the digests travel with the pack and no installer code change is required for v1. The one behavior the future engine must respect (contract §1.2 currently says the engine sets `<comp>.image.repository`/`.tag`): it **must not override a digest-pinned reference**. Document that in the contract.

## 7. Scripts & Makefile

### 7.1 `scripts/collect-image-digests.sh` (new, optional)
Reads `<component>=<ref>` lines (or `digest-*.txt` args) and emits the JSON map. Keeps the merge logic out of inline YAML and reusable locally.

### 7.2 `scripts/pin-installer-values.py` (new)
Inputs: `values-installer.yaml` path + digests map + `--require-all`. Rewrites the image block per §6 using PyYAML; prints the pinned refs; non-zero on a missing required image.

### 7.3 `scripts/check-release-version.sh` (new)
Asserts `<arg>` equals `Chart.yaml` `version`, `Chart.yaml` `appVersion` (quotes stripped) and `ui/package.json` `version`; prints a clear mismatch error.

### 7.4 `scripts/build-install-pack.sh` (modify)
Accept `IMAGE_DIGESTS` / `IMAGE_DIGESTS_FILE`; after copying the chart, invoke `scripts/pin-installer-values.py` on the copied `values-installer.yaml` when a map is given; add `images` to `metadata.json`; keep the existing template/Cilium/checksum gates.

### 7.5 `scripts/audit.sh` (modify)
Add a `check_image_digest_render` assertion (helm template with `--set <comp>.image.digest=sha256:<64hex>` renders `repository@sha256:…` for each chart component). Add a `check_pack_digest_pinning` assertion that runs `build-install-pack.sh` with a synthetic digests file (fake but well-formed `sha256:` values) and asserts the pack's `values-installer.yaml` carries all 7 digests, that `metadata.json.images` matches, and that a missing entry fails the build. This is the test paired with the new spec MUST (§8).

### 7.6 `Makefile` (modify)
- `IMAGE_DIGESTS_FILE ?=` and pass it through `install-pack` → `scripts/build-install-pack.sh`.
- Optional `image-digests:` note that digests for GHCR come from `docker buildx imagetools inspect` (the existing `image-digests` target uses `docker inspect`, private-registry oriented).
- Update the header comment about CI (it currently implies no CI).

## 8. Docs, spec, credits (same change — AGENTS "Docs rule"/"Spec rule"/"Credits rule")

- `docs/installer-contract.md`:
  - §1: the pack's `values-installer.yaml` pins every chart image by digest from the public registry `ghcr.io/aessemops/naslos-*`; tag-only refs are local/dev only.
  - §1.2: the engine MUST NOT override a digest-pinned image reference (clarify the existing repository/tag row).
  - §1 metadata table: note the additive `images` field.
- `docs/spec.md`: add **FR-INSTALL-13** — a released pack (`vX.Y.Z`) MUST pin each of the 7 chart images by digest to the public registry, and the tag workflow MUST publish those images before the pack; `[OPEN]`-free once the gate lands. Map it to the `audit.sh` pack-pinning check in the verification table. (FR-INSTALL-08 already exists — recovery ZIP; 13 is the next free id.)
- `docs/deployment.md`: a short "GHCR images + release tags" section (public packages, `vX.Y.Z` → images+pack, the manual visibility step).
- `AI_Handoff.md`: replace the "images are tag-pinned placeholders / residual NAS-022" note with the CI behavior; note the open `INSTALLER_DISPATCH_TOKEN` and public-package prerequisites; add to "Deployed right now" only when actually released.
- `README.md` (README rule): one line under Features/Layout for the GHCR images + release CI and the `docs/installer-contract.md` linkage.
- `AGENTS.md`: add the CI commands (what runs where), the release order (tag → images → pack → installer dispatch), and the "never push a throwaway strict-semver tag" rule.
- `CREDITS.md`: add the new GitHub Actions (`docker/login-action`, `docker/setup-buildx-action`, `docker/build-push-action`, `gitleaks/gitleaks-action`); bump `Last reviewed`.

## 9. Validation

- `bash scripts/audit.sh` green locally (including the two new checks).
- `helm lint`/`helm template` both profiles; `helm template` with a digests file renders `@sha256:` for all 7 components.
- `make install-pack PACK_VERSION=0.1.0` still produces a checksum-valid pack (no digests).
- `make install-pack PACK_VERSION=0.1.0 IMAGE_DIGESTS_FILE=<synthetic>` produces a pack whose `values-installer.yaml` has all 7 digests and whose `metadata.json.images` matches; a missing component fails.
- `scripts/check-release-version.sh 0.1.0` passes on the current tree; `… 0.2.0` fails.
- `ci.yml` exercises itself on the PR (audit + gitleaks + images-build + pack).
- **Release dry run (no publish):** Actions → `release.yml` → Run workflow with `version=0.1.0`, `publish=false`. Confirms image push, digest collection, digest-pinned pack build, and pack upload as an artifact, without touching the GitHub release or dispatching the installer. **Do not** use a scratch strict `vX.Y.Z` tag.
- Optional: `act` locally for the workflows before the PR.

## 10. Prerequisites / operator actions (documented, not code)

1. **`INSTALLER_DISPATCH_TOKEN`** secret (already required by `install-pack.yml`, not configured): fine-grained PAT with `Contents: read and write` (or `Actions: write`) on `AessemOps/Naslos-Installer`. Without it, images + pack publish; the installer rebuild is skipped.
2. **Make the 8 GHCR packages public** (one-time, per package): GitHub → org package settings → visibility → Public (or an org "package creation" default). `GITHUB_TOKEN` + `packages: write` can **publish** but cannot change visibility, so automation needs a PAT with package-admin rights (`GHCR_PACKAGES_TOKEN`, optional step). Document the manual route; automate later.
3. **Branch protection**: require `ci / audit`, `ci / gitleaks`, `ci / images-build` on `master` PRs (optional).
4. Bump `Chart.yaml` `version`/`appVersion` and `ui/package.json` `version` in the release commit (enforced by `guard`).

## 11. Risks / open items

- **Public packages**: required for an anonymous node pull. If a future deployment needs private packages, that is a new contract item (imagePullSecret) — out of scope here.
- **Known trivy/trixie base CVEs**: deliberately **not** gated in CI (would be red from documented no-fix base libraries). Optional advisory `trivy` job with SARIF upload + `continue-on-error`; not blocking.
- **Playwright** is live-VM/authenticated and stays out of CI (documented exclusion); UI/API regressions are covered by `audit.sh` + Docker builds.
- **Digest-pin vs engine override**: the one installer-side behavior to honor later (contract §1.2). The installer's helm step is unimplemented today, so this is documentation now, not a code change.
- **Strict-semver pollution**: any strict `vX.Y.Z` tag becomes the installer's "newest". Use `workflow_dispatch` for tests.

## 12. Task list (ordered)

1. `scripts/pin-installer-values.py`, `scripts/collect-image-digests.sh`, `scripts/check-release-version.sh`; wire `IMAGE_DIGESTS_FILE` through `Makefile` → `scripts/build-install-pack.sh`; add the `images` map to `metadata.json`.
2. Add `check_image_digest_render` + `check_pack_digest_pinning` to `scripts/audit.sh`; run `bash scripts/audit.sh` green.
3. Add `.github/workflows/ci.yml` (audit, gitleaks, images-build matrix, pack), actions SHA-pinned.
4. Add `.github/workflows/release.yml` (guard → images matrix w/ digests → digest-pinned pack → release → dispatch); delete `.github/workflows/install-pack.yml`.
5. Dry-run `release.yml` via `workflow_dispatch` (`publish=false`); confirm digests, pack, and rendering.
6. Docs/spec/credits: `docs/installer-contract.md`, `docs/spec.md` (FR-INSTALL-13 + verification row), `docs/deployment.md`, `AGENTS.md`, `README.md`, `AI_Handoff.md`, `CREDITS.md`.
7. Commit on `ci/ghcr-image-release`, open the PR, verify the required checks, then (with user approval) make the GHCR packages public and create the first real `vX.Y.Z` tag.
