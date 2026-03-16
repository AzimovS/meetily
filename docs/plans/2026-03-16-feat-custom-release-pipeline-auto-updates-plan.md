---
title: "feat: Custom Release Pipeline, Versioning, and Auto-Updates"
type: feat
status: completed
date: 2026-03-16
---

# Custom Release Pipeline, Versioning, and Auto-Updates

## Enhancement Summary

**Deepened on:** 2026-03-16
**Sections enhanced:** All major sections
**Research agents used:** Security Sentinel, Deployment Verification, Architecture Strategist, Performance Oracle, Code Simplicity Reviewer, latest.json Merge Research, GitHub Actions Best Practices Research

### Key Improvements
1. **Simplified from 16 tasks / 3 phases to 5 tasks / 2 passes** — cut version bump script, PR validation, release checklist, FFmpeg fork as YAGNI for a solo developer
2. **Resolved latest.json race condition** — `tauri-action` merges (not overwrites) but has a known race with parallel builds (issue #1270). Added sequential build strategy as primary approach.
3. **Added security hardening** — SHA-256 checksums for FFmpeg downloads, pin GitHub Actions to commit SHAs, explicit secret passing instead of `secrets: inherit`, key backup strategy
4. **Resolved license validation mystery** — `MEETILY_RSA_PUBLIC_KEY` has zero references in Rust source code; it's vestigial and safe to omit
5. **Added CI performance optimizations** — sccache, thin LTO for llama-helper, Vulkan SDK caching, parallel pnpm + cargo builds
6. **Added deployment checklist and rollback procedures** — full Go/No-Go checklist with verification commands

### New Considerations Discovered
- Apple cert import steps in `build.yml` will **fail** when `APPLE_CERTIFICATE` secret is empty and `sign-binaries: true` — must make conditional
- 7 workflow files with massive duplication (~1500 lines) — consolidate to 4 after release works
- `tauri-action@v0` tracks a mutable tag — pin to commit SHA for supply chain safety
- `appimagetool` downloaded from a `continuous` rolling tag with no checksum verification

---

## Overview

Fully decouple Meetily's release pipeline from the upstream `Zackriya-Solutions/meeting-minutes` repository. This includes: generating new updater signing keys, updating the auto-update endpoint to `AzimovS/meetily`, resetting the version to `v0.1.0`, adding Linux to the release matrix, and making code signing conditional. Clean break — no migration bridge for existing Zackriya users.

## Problem Statement

The current release pipeline is configured for the upstream Zackriya-Solutions repository:
- **Updater endpoint** (`tauri.conf.json:117`) points to `https://github.com/Zackriya-Solutions/meeting-minutes/releases/latest/download/latest.json`
- **Ed25519 public key** (`tauri.conf.json:115`) is Zackriya's key — we cannot sign releases with their private key
- **Version** is `0.3.0` — continuing upstream's sequence
- **Branding** references "Zackriya Solutions" in About page, Cargo.toml, and setup flows
- **Linux** is excluded from the release pipeline
- **FFmpeg binaries** are downloaded from `Zackriya-Solutions/ffmpeg-binaries` releases during build — **with no integrity verification** (Critical security finding)
- **Code signing**: No Apple Developer ID or Windows code signing certificate available yet

Publishing releases from `AzimovS/meetily` is impossible without replacing the signing keys and endpoint.

### Research Insights: Security Posture

**Critical findings from security audit:**
- FFmpeg binaries downloaded at build time have **zero cryptographic integrity verification** — only `ffmpeg -version` is checked, which proves execution but not authenticity. A compromised upstream repo could inject trojanized binaries into every build.
- The `secrets: inherit` pattern in `release.yml` and `build-test.yml` exposes ALL repository secrets to the reusable workflow, violating least-privilege.
- All third-party GitHub Actions are pinned to mutable version tags (`@v4`, `@v0`) rather than commit SHAs — a supply chain risk if any action author's account is compromised.
- `appimagetool` is downloaded from a `continuous` rolling release tag with no checksum.

**Resolved: License validation env vars are vestigial.** Grep of `frontend/src-tauri/src/` found zero references to `MEETILY_RSA_PUBLIC_KEY`, `SUPABASE_URL`, or `SUPABASE_ANON_KEY` in Rust source code. These env vars are passed to the build but never consumed. Safe to omit.

## Proposed Solution

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│              GitHub Actions (AzimovS/meetily)                    │
│                                                                  │
│  workflow_dispatch → release.yml                                │
│       ├─ Create draft GitHub Release (tag v0.1.0)               │
│       ├─ Build macOS (aarch64-apple-darwin)  ─┐                 │
│       ├─ Build Windows (x86_64-pc-windows-msvc) ─┤  sequential  │
│       └─ Build Linux (x86_64-unknown-linux-gnu) ─┘  (needs:)    │
│            │                                                     │
│            └─ tauri-action: build + sign + upload artifacts      │
│                 ├─ .dmg / .app.tar.gz + .sig (macOS)            │
│                 ├─ .msi / .exe + .sig (Windows)                 │
│                 ├─ .AppImage + .sig (Linux)                     │
│                 └─ latest.json (merged across all platforms)    │
│                                                                  │
│  Developer reviews draft → Publishes release                    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│              Auto-Update Flow (installed app)                    │
│                                                                  │
│  App starts → 2s delay → check() fetches:                       │
│  https://github.com/AzimovS/meetily/releases/latest/            │
│  download/latest.json                                            │
│       │                                                          │
│       ├─ No update → silent, retry in 24h                       │
│       └─ Update available → toast notification                  │
│            └─ User clicks → UpdateDialog                        │
│                 └─ downloadAndInstall() → progress bar          │
│                      └─ relaunch()                              │
└─────────────────────────────────────────────────────────────────┘
```

### Research Insights: latest.json Merge Behavior

**Critical finding**: `tauri-action@v0` implements a **download-merge-delete-reupload** strategy for `latest.json` (source: `src/upload-version-json.ts`). It does NOT overwrite — it downloads the existing `latest.json`, preserves all platform entries, adds the current platform's entry, then re-uploads.

**However, there is a known race condition** (open issue [#1270](https://github.com/tauri-apps/tauri-action/issues/1270)): when parallel matrix jobs finish near-simultaneously, both download the same `latest.json`, each adds its platform, and the last uploader's version wins — losing the other platform's entry.

**Recommended approach**: Use **sequential builds via `needs:`** to eliminate the race condition entirely. This adds ~10 min total build time but guarantees correct `latest.json`. Alternative: set `retryAttempts: 3` and accept occasional flakiness, or add a post-build merge job with `uploadUpdaterJson: false`.

```yaml
# Sequential approach (recommended for reliability)
jobs:
  build-macos:
    needs: create-release
    ...
  build-windows:
    needs: [create-release, build-macos]
    ...
  build-linux:
    needs: [create-release, build-windows]
    ...
```

---

## Technical Approach — Simplified (2 Passes, 5 Tasks)

### Research Insights: Simplification Analysis

The original 16-task / 3-phase plan was reviewed for YAGNI violations. For a solo developer, the following were cut:
- **Version bump script** — you bump versions maybe once a week; editing 3 files by hand takes 30 seconds
- **PR version sync check** — solo developer guardrail for a team-scale problem
- **Release checklist template** — process artifact with no audience
- **FFmpeg repo fork** — maintain later only if the upstream actually breaks
- **`latest.json` pre-verification task** — observable on first real release

### Pass 1: Foundation (Do First, in One Sitting)

#### Task 1: Generate Signing Keys and Add GitHub Secrets

```bash
cd frontend
npx @tauri-apps/cli signer generate -w ~/.tauri/meetily-azimov.key
```

This produces:
- **Private key**: `~/.tauri/meetily-azimov.key` (NEVER commit)
- **Public key**: printed to stdout and saved as `~/.tauri/meetily-azimov.key.pub`

**GitHub Secrets to add** (Settings → Secrets and variables → Actions):

| Secret | Value | Required |
|--------|-------|----------|
| `TAURI_SIGNING_PRIVATE_KEY` | Contents of `~/.tauri/meetily-azimov.key` | YES — builds fail without it |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Password chosen during generation (or empty string) | YES |

All other secrets (Apple, DigiCert, Supabase, RSA key) are **not needed** — the workflow must skip those steps gracefully.

##### Research Insights: Key Backup Strategy

**If the Ed25519 private key is lost, every installed copy of the app becomes permanently unable to receive signed updates. There is no recovery.**

**Tiered backup (do immediately after generation):**

| Tier | Method | Details |
|------|--------|---------|
| 1 — Password Manager | 1Password or Bitwarden Secure Note | Store: private key contents, password, public key, date generated, which GitHub repos reference it |
| 2 — Encrypted Offline | `age -p -o tauri-keys.age ~/.tauri/meetily-azimov.key` | Store encrypted file on separate device. Keep `age` passphrase separate from the file. |

**Key password policy**: Use a strong password (minimum 20 characters, randomly generated). If the key is exfiltrated from GitHub Actions logs, a weak password means immediate compromise.

**Future key rotation procedure**: Ship a release signed with the old key that contains the new public key in `tauri.conf.json`, then switch fully to the new key in the next release.

#### Task 2: Single Commit — Config Changes

Update all of the following in **one commit**:

| File | Change |
|------|--------|
| `frontend/src-tauri/tauri.conf.json:4` | `"version": "0.1.0"` |
| `frontend/src-tauri/tauri.conf.json:115` | Replace `pubkey` with new public key string |
| `frontend/src-tauri/tauri.conf.json:117` | Change endpoint to `https://github.com/AzimovS/meetily/releases/latest/download/latest.json` |
| `frontend/src-tauri/Cargo.toml:3` | `version = "0.1.0"` |
| `frontend/src-tauri/Cargo.toml:7` | `repository = "https://github.com/AzimovS/meetily"` |
| `frontend/package.json:3` | `"version": "0.1.0"` |

**No need to audit `MEETILY_RSA_PUBLIC_KEY`** — confirmed zero references in Rust source code. It's vestigial.

##### Research Insights: Security — Update Endpoint

Changing the endpoint is a **Critical security fix**. The upstream repo owner currently controls what `latest.json` is served to your users. Even though the Ed25519 signature check prevents code injection (the pubkey must match), the upstream can cause denial-of-service by publishing malformed `latest.json` pointing to non-existent URLs.

#### Task 3: Remove Version Auto-Increment from CI

The current `release.yml` (lines 29-67) auto-appends `.N` suffixes (e.g., `v0.3.0.1`). This creates four-segment versions that may break Tauri's semver comparison.

**Replace** the entire auto-increment block with:

```yaml
- name: Get and validate version
  id: get-version
  shell: bash
  run: |
    VERSION=$(grep -o '"version": "[^"]*"' frontend/src-tauri/tauri.conf.json | cut -d'"' -f4)

    # Validate semver format
    if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$'; then
      echo "::error::Version '$VERSION' is not valid semver"
      exit 1
    fi

    git fetch --tags
    if git tag -l "v${VERSION}" | grep -q .; then
      echo "::error::Tag v${VERSION} already exists. Bump the version before releasing."
      exit 1
    fi

    echo "version=$VERSION" >> "$GITHUB_OUTPUT"
```

##### Research Insights: Conditional Code Signing

The Apple certificate import steps in `build.yml:441-462` will **fail** when `APPLE_CERTIFICATE` is empty and `sign-binaries: true`. The base64 decode step receives an empty string and errors out.

**Fix — Pattern for conditional signing:**

```yaml
# In release.yml: detect secret availability
jobs:
  check-secrets:
    runs-on: ubuntu-latest
    outputs:
      has-apple-signing: ${{ steps.check.outputs.has-apple }}
    steps:
      - id: check
        run: echo "has-apple=${{ secrets.APPLE_CERTIFICATE != '' }}" >> "$GITHUB_OUTPUT"

  build-all-platforms:
    needs: [create-release, check-secrets]
    uses: ./.github/workflows/build.yml
    with:
      sign-binaries: ${{ needs.check-secrets.outputs.has-apple-signing == 'true' }}
    secrets: inherit  # TODO: replace with explicit secret passing (see security notes)
```

**Additionally**, add a guard inside `build.yml` for each signing step:

```yaml
- name: Import Apple Developer Certificate
  if: contains(inputs.platform, 'macos') && inputs.sign-binaries && env.APPLE_CERTIFICATE != ''
  env:
    APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
  run: |
    # existing certificate import logic
```

### Pass 2: Linux and Test Release

#### Task 4: Add Linux to Release Matrix

Add `ubuntu-22.04` to the `release.yml` matrix. The reusable `build.yml` already handles Linux builds (system deps, OpenBLAS, libwayland-client removal, AppImage generation).

**Use sequential builds to avoid the latest.json race condition:**

```yaml
strategy:
  fail-fast: false
  matrix:
    include:
      - platform: "macos-latest"
        args: "--target aarch64-apple-darwin"
        target: "aarch64-apple-darwin"
        build_features: "metal"
      - platform: "windows-latest"
        args: "--target x86_64-pc-windows-msvc"
        target: "x86_64-pc-windows-msvc"
        build_features: "vulkan"
      - platform: "ubuntu-22.04"
        args: ""
        target: "x86_64-unknown-linux-gnu"
        build_features: "openblas"
```

**If using matrix (parallel)**: Set `retryAttempts: 3` on `tauri-action` and verify `latest.json` contains all 3 platforms before publishing. If a platform is missing, use the manual merge fallback (see Rollback section).

**If using sequential `needs:` chain**: No race condition, but ~10 min slower total.

**Key consideration**: For auto-updates, only AppImage is supported on Linux. Ensure `createUpdaterArtifacts: true` generates `.AppImage.tar.gz` + `.AppImage.tar.gz.sig`. The `latest.json` entry for `linux-x86_64` should point to the AppImage tarball.

#### Task 5: Test a Real Release

Push the changes, trigger the release workflow, and verify:

1. All 3 platform builds complete
2. Draft release contains all expected artifacts
3. `latest.json` has entries for `darwin-aarch64`, `windows-x86_64`, `linux-x86_64`
4. Install v0.1.0 on at least one platform
5. Bump to v0.1.1, release, verify auto-update works end-to-end

##### Research Insights: Post-Release Verification Commands

```bash
# List all assets in the draft release
gh release view v0.1.0 --repo AzimovS/meetily --json assets --jq '.assets[].name'

# Validate latest.json structure
gh release download v0.1.0 --repo AzimovS/meetily --pattern "latest.json" --dir /tmp
python3 -c "
import json, sys
data = json.load(open('/tmp/latest.json'))
required = ['darwin-aarch64', 'windows-x86_64', 'linux-x86_64']
for p in required:
    assert p in data.get('platforms', {}), f'MISSING: {p}'
    assert data['platforms'][p].get('signature'), f'NO SIG: {p}'
    assert data['platforms'][p].get('url'), f'NO URL: {p}'
    print(f'OK: {p}')
print(f'Version: {data[\"version\"]}')
"

# Verify the "latest" redirect works after publishing
curl -sI https://github.com/AzimovS/meetily/releases/latest | grep location
```

---

## Deferred Work (Do When Needed, Not Now)

These items were cut from the main plan as YAGNI but are documented for when they become relevant:

| Item | When to Do It | Effort |
|------|--------------|--------|
| Version bump script (`scripts/bump-version.sh`) | When you find yourself bumping versions frequently and making mistakes | 15 min |
| PR version sync check in `pr-main-check.yml` | When you have contributors and version drift becomes a real problem | 10 min |
| Fork FFmpeg binaries repo | When/if Zackriya deletes their repo and builds break | 30 min |
| Update branding (About.tsx, SetupOverviewStep.tsx) | Whenever you feel like it — cosmetic | 5 min |
| Consolidate 7 workflow files to 4 | After the release pipeline works — removes ~1500 lines of duplicated YAML | 2 hr |
| Pin GitHub Actions to commit SHAs | Before any public/widely-distributed release | 30 min |
| Replace `secrets: inherit` with explicit passing | Before any public/widely-distributed release | 15 min |
| Add SHA-256 checksums for FFmpeg in `build/ffmpeg.rs` | Before any public/widely-distributed release | 30 min |
| Reduce `contents: write` to `contents: read` for non-release workflows | Cleanup pass | 10 min |

### Research Insights: Workflow Consolidation (Architecture)

The codebase has 7 workflow files with massive duplication:
- `build-macos.yml` (282 lines), `build-windows.yml` (733 lines), `build-linux.yml` (347 lines) all duplicate logic from `build.yml` (680 lines)
- DigiCert setup, Apple cert import, Linux deps, Vulkan SDK, FFmpeg cache, AppImage post-processing are each copied 2-4 times
- Vulkan SDK version `1.4.309.0` is hardcoded in 4 separate places

**After the release pipeline works**: Delete `build-macos.yml`, `build-windows.yml`, `build-linux.yml`. Refactor `build-devtest.yml` to call `build.yml` like `build-test.yml` already does. This reduces surface from 7 files to 4.

---

## CI Performance Optimizations

### Research Insights: Build Time Reduction

The Performance Oracle identified several bottlenecks. These are optional improvements to apply after the pipeline works:

**High Impact, Low Effort:**

1. **Add sccache** — 15-30% build time reduction on warm caches, especially for whisper.cpp C++ compilation:
```yaml
- uses: mozilla-actions/sccache-action@v0.0.6
- run: |
    echo "SCCACHE_GHA_ENABLED=true" >> $GITHUB_ENV
    echo "RUSTC_WRAPPER=sccache" >> $GITHUB_ENV
```

2. **Use `lto = "thin"` for llama-helper** — saves 2-4 min link time per platform. In `llama-helper/Cargo.toml`:
```toml
[profile.release]
codegen-units = 1
lto = "thin"  # was: lto = true (fat LTO)
```

3. **Pin FFmpeg cache key to version** — more stable cache hits:
```yaml
key: ${{ runner.os }}-${{ inputs.target }}-ffmpeg-v8.0.1
# was: ${{ runner.os }}-ffmpeg-${{ hashFiles('...') }}
```

**Moderate Impact, Moderate Effort:**

4. **Cache Vulkan SDK on Linux** — saves 2-4 min per Linux build (currently installed via `apt` every time)
5. **Parallelize pnpm install and llama-helper build** — saves 1-2 min per platform (no dependency between them)
6. **Cache APT packages** on Linux — avoids re-downloading ~500MB of system dependencies

**Runner Performance Reference:**

| Platform | Runner | vCPUs | Notes |
|----------|--------|-------|-------|
| macOS (ARM) | macos-latest | 3 cores (M1) | Fastest for Rust |
| Windows | windows-latest | 2 cores | Slowest (MSVC linker + Defender scanning) |
| Linux | ubuntu-22.04 | 2 cores | Middle ground |

---

## Security Hardening Checklist

### Research Insights: Prioritized Remediation

**Do before first public release:**

- [ ] Change update endpoint from Zackriya to AzimovS (Critical — upstream controls update delivery)
- [ ] Back up Ed25519 private key to password manager (Critical — irrecoverable if lost)
- [ ] Make Apple cert steps conditional on secret being non-empty (High — builds will fail otherwise)

**Do before wide distribution:**

- [ ] Add SHA-256 checksums for FFmpeg downloads in `build/ffmpeg.rs` (Critical supply chain risk)
- [ ] Pin all GitHub Actions to commit SHAs instead of version tags (High — supply chain risk)
- [ ] Replace `secrets: inherit` with explicit secret passing in `release.yml` and `build-test.yml` (High)
- [ ] Reduce `contents: write` to `contents: read` for non-release workflows (High)
- [ ] Pin `appimagetool` download to a specific release with SHA-256 verification (Medium)

**Example: FFmpeg checksum verification in `build/ffmpeg.rs`:**
```rust
// After downloading, before extraction:
let expected_sha256 = match target.as_str() {
    "x86_64-pc-windows-msvc" => "abc123...",
    "aarch64-apple-darwin" => "def456...",
    // ...
};
let actual = sha256::digest(&downloaded_bytes);
assert_eq!(actual, expected_sha256, "FFmpeg binary checksum mismatch — possible supply chain compromise");
```

---

## Alternative Approaches Considered

### 1. Self-Hosted Update Server (Rejected)
Adds infrastructure complexity and cost. GitHub Releases is free, reliable, CDN-backed, and sufficient for the current user base.

### 2. S3/CloudFront for Updates (Rejected)
Same artifacts can be served from GitHub Releases for free.

### 3. Universal macOS Binary (Deferred)
Cross-compiling native C libraries (whisper.cpp, cpal, FFmpeg) from ARM to x86_64 is complex. Intel Mac market share is declining.

### 4. Bridge Release for Zackriya Users (Rejected)
User confirmed clean break is acceptable.

### 5. Keep Four-Segment Version Auto-Increment (Rejected)
Tauri's updater uses semver comparison. Four-segment versions are non-standard and may cause update detection failures.

### 6. Parallel Matrix Builds with Post-Build Merge (Considered)

**Approach**: Run all 3 platforms in parallel with `uploadUpdaterJson: false`, then add a final job that constructs `latest.json` from release assets.

**Trade-off**: Faster (~10 min) but more complex workflow. The sequential `needs:` approach is simpler and `tauri-action`'s built-in merge works reliably when builds run one at a time.

**Verdict**: Start with sequential. Switch to parallel + post-build merge if build times become painful.

---

## System-Wide Impact

### Interaction Graph

1. Developer triggers `release.yml` → creates draft release → calls `build.yml` sequentially for 3 platforms
2. `build.yml` runs `tauri build` → `build/ffmpeg.rs` downloads FFmpeg → `tauri-action` uploads artifacts + merges `latest.json` to release
3. Developer publishes release → GitHub "latest" points to new release
4. Installed app: `UpdateCheckProvider` mounts → `useUpdateCheck` fires after 2s → `updateService.checkForUpdates()` → Tauri `check()` fetches `latest.json` from `AzimovS/meetily` → compares version → shows notification → user downloads + installs → `relaunch()`

### Error Propagation

- **Missing signing key**: `tauri build` fails with "TAURI_SIGNING_PRIVATE_KEY not set" → build job fails → no artifacts uploaded → draft release is empty
- **Empty `APPLE_CERTIFICATE` with `sign-binaries: true`**: base64 decode fails → macOS build fails entirely → **must make conditional** (see Task 3)
- **FFmpeg download failure**: `build/ffmpeg.rs` panics → `cargo build` fails → entire platform build fails
- **Signature mismatch on client**: Tauri updater rejects update silently → user sees "no update available"

### State Lifecycle Risks

- **Draft release never published**: `latest.json` is uploaded but "latest" release still points to old version. Users never see the update.
- **Key loss**: If the Ed25519 private key is lost, no future updates can be signed. All installed apps must be manually re-downloaded. **Back up immediately.**
- **Abandoned drafts**: If a draft for v0.2.0 is created but abandoned (not deleted), the "latest" release remains at the previous version. Harmless but messy — consider deleting abandoned drafts.

### API Surface Parity

The auto-update flow uses the same Tauri updater plugin across all touchpoints (`tauri.conf.json`, `updateService.ts`, `UpdateDialog.tsx`, `UpdateNotification.tsx`, `About.tsx`, `lib.rs:394`). All will work with the new endpoint/keys without code changes — only config changes needed.

### Integration Test Scenarios

1. **Full update cycle**: Build v0.1.0, install, build v0.1.1, publish, verify auto-update on macOS
2. **Signature rejection**: Build with one keypair, configure app with different public key, verify update rejected
3. **Linux AppImage update**: Install AppImage v0.1.0, publish v0.1.1, verify auto-update
4. **Missing platform**: Remove `linux-x86_64` from `latest.json`, verify Linux app shows "no update" gracefully
5. **Network failure**: Start update download, disconnect network, verify error shown and app remains functional

---

## Acceptance Criteria

### Functional Requirements

- [ ] `tauri signer generate` creates new Ed25519 keypair; public key in `tauri.conf.json`, private key in GitHub Secrets
- [ ] Auto-update endpoint points to `https://github.com/AzimovS/meetily/releases/latest/download/latest.json`
- [ ] All three version files (`tauri.conf.json`, `Cargo.toml`, `package.json`) show `0.1.0`
- [ ] `release.yml` builds for macOS (aarch64), Windows (x64), and Linux (x64) via `workflow_dispatch`
- [ ] Published release contains: `.dmg`, `.app.tar.gz`, `.msi`, `.exe`, `.AppImage`, `.deb`, plus all `.sig` files and `latest.json`
- [ ] `latest.json` contains valid entries for `darwin-aarch64`, `windows-x86_64`, and `linux-x86_64`
- [ ] Installed app on macOS/Windows/Linux (AppImage) detects and installs updates from the new endpoint
- [ ] Code signing steps are conditional — workflow succeeds without Apple/DigiCert secrets
- [ ] Ed25519 private key backed up to password manager (1Password/Bitwarden)

### Non-Functional Requirements

- [ ] Release workflow completes in under 45 minutes for all platforms (sequential builds)
- [ ] No hardcoded references to `Zackriya-Solutions` remain in `tauri.conf.json` updater config
- [ ] Ed25519 private key is backed up in at least one location outside GitHub Secrets

### Quality Gates

- [ ] Test release published and auto-update verified on at least one platform (v0.1.0 → v0.1.1 cycle)
- [ ] `latest.json` validated with the verification script in Task 5

---

## Rollback Procedures

### Before Publishing (Draft Release)

1. Delete the draft release on GitHub
2. Delete the tag: `git push --delete origin v0.1.0`
3. Fix the issue
4. Re-trigger the workflow

### latest.json Missing Platforms (Manual Merge)

If `latest.json` is incomplete after parallel builds:

```bash
# Construct correct latest.json from release assets
cat > /tmp/latest.json << 'EOF'
{
  "version": "0.1.0",
  "notes": "First independent release",
  "pub_date": "2026-03-16T00:00:00Z",
  "platforms": {
    "darwin-aarch64": {
      "signature": "<PASTE .app.tar.gz.sig CONTENTS>",
      "url": "https://github.com/AzimovS/meetily/releases/download/v0.1.0/meetily.app.tar.gz"
    },
    "windows-x86_64": {
      "signature": "<PASTE .nsis.zip.sig CONTENTS>",
      "url": "https://github.com/AzimovS/meetily/releases/download/v0.1.0/meetily_0.1.0_x64-setup.nsis.zip"
    },
    "linux-x86_64": {
      "signature": "<PASTE .AppImage.tar.gz.sig CONTENTS>",
      "url": "https://github.com/AzimovS/meetily/releases/download/v0.1.0/meetily_0.1.0_amd64.AppImage.tar.gz"
    }
  }
}
EOF

# Upload to release (overwrites existing)
gh release upload v0.1.0 /tmp/latest.json --repo AzimovS/meetily --clobber
```

### After Publishing (Bad Release)

1. Delete the release entirely
2. The `latest.json` URL returns 404
3. Installed apps silently fail the update check (errors are caught in `updateService.ts:80-83`)
4. Users keep running their current version — no harm done
5. Fix the issue and publish a new version

---

## Success Metrics

- First release (`v0.1.0`) successfully published with all three platform artifacts
- Auto-update verified: app running `v0.1.0` detects and installs `v0.1.1`
- Build time per platform under 20 minutes with caching

## Dependencies & Prerequisites

| Dependency | Status | Blocking? |
|------------|--------|-----------|
| GitHub repo `AzimovS/meetily` | Exists | No |
| Ed25519 signing keypair | Must generate | Yes (Task 1) |
| `TAURI_SIGNING_PRIVATE_KEY` secret | Must add | Yes (Task 1) |
| Apple Developer ID certificate | Not available | No (conditional, skipped) |
| Windows code signing certificate | Not available | No (conditional, skipped) |

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Lost Ed25519 private key | Low | Critical | Back up to password manager + encrypted offline copy immediately |
| Apple cert steps fail when secrets empty | Certain (if not fixed) | High | Make conditional in build.yml (Task 3) |
| `latest.json` race in parallel builds | Medium | High | Use sequential builds via `needs:` or manual merge fallback |
| FFmpeg supply chain compromise | Low | Critical | Add SHA-256 checksums before public distribution |
| macOS Gatekeeper blocks unsigned app | Certain | Medium | Document right-click → Open workaround in release notes |
| Windows SmartScreen warns on unsigned installer | Certain | Medium | Document "More info → Run anyway" workaround |
| GitHub Action supply chain (mutable tags) | Low | High | Pin to commit SHAs before public distribution |

## Future Considerations

1. **Apple Developer ID Certificate**: When obtained, add secrets — the workflow is already configured to use them
2. **Windows Code Signing**: When obtained, add DigiCert secrets — `sign-windows.ps1` is already in place
3. **macOS Intel (x86_64) builds**: Add a second macOS matrix entry when needed
4. **Staged rollouts**: Deploy a lightweight update proxy on Fly.io
5. **Changelog generation**: Use `git-cliff` for auto-generated release notes
6. **Linux ARM64**: Add `ubuntu-22.04-arm` matrix entry
7. **Workflow consolidation**: Delete standalone platform workflows, reduce to 4 files

## Sources & References

### Internal References

- Current updater config: `frontend/src-tauri/tauri.conf.json:113-120`
- Release workflow: `.github/workflows/release.yml`
- Build workflow: `.github/workflows/build.yml`
- Linux build: `.github/workflows/build-linux.yml`
- FFmpeg download URLs: `frontend/src-tauri/build/ffmpeg.rs:129-144`
- Update UI: `frontend/src/services/updateService.ts`, `frontend/src/components/UpdateDialog.tsx`
- Windows signing: `frontend/src-tauri/scripts/sign-windows.ps1`
- Workflow overview: `.github/workflows/WORKFLOWS_OVERVIEW.md`
- llama-helper LTO config: `llama-helper/Cargo.toml:19-22`

### External References

- [Tauri 2.x Updater Plugin](https://v2.tauri.app/plugin/updater/)
- [Tauri GitHub Actions Pipeline](https://v2.tauri.app/distribute/pipelines/github/)
- [Tauri macOS Code Signing](https://v2.tauri.app/distribute/sign/macos/)
- [Tauri Windows Code Signing](https://v2.tauri.app/distribute/sign/windows/)
- [tauri-apps/tauri-action](https://github.com/tauri-apps/tauri-action)
- [tauri-action issue #1270 — latest.json race condition](https://github.com/tauri-apps/tauri-action/issues/1270)
- [Tauri Signer CLI](https://v2.tauri.app/reference/cli/#signer-generate)
- [GitHub Actions cache can exceed 10 GB (Nov 2025)](https://github.blog/changelog/2025-11-20-github-actions-cache-size-can-now-exceed-10-gb-per-repository/)
- [sccache in GitHub Actions](https://depot.dev/blog/sccache-in-github-actions)
