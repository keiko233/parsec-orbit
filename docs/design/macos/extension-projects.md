# Extension-project research: Spicetify and alternative entry paths

Date: 2026-10-13. Scope: macOS arm64, client only. This investigation preceded the [native dependency PoC](native-dependency-poc.md); its candidate assessment is historical, not runtime proof. Current design: [ADR 0014](../decisions/0014-macos-patched-bundle.md).

## Finding

Spicetify's reusable idea is a version-aware install-time patch, followed by ordinary application launch, explicit re-apply after updates and restore on removal. Its resource-JS implementation does not implement Parsec's native plugin ABI. The leading Parsec candidate was an added Mach-O dependency in the small bootstrap, leaving vendor client instructions/hash validation intact and re-signing the modified outer app. The follow-up PoC validated this entry in a disposable copy, with important signing/distribution boundaries.

The earlier [launch-integration investigation](launch-integration.md) assumed the official bundle and signature could not change. Its negative conclusion remains valid under that constraint; it does not exclude the now-permitted patch-and-re-sign route. Launch watchers, global DYLD settings and duplicate bundle ids need not be rediscovered.

## Spicetify implementation

Pinned source: `spicetify/cli` at `16840237b5c4f0edfc0556b9c214f8c2029b6390`.

1. macOS discovery selects `/Applications/Spotify.app/Contents/Resources`, including `Apps`. This directly targets app resources. [config.go](https://github.com/spicetify/cli/blob/16840237b5c4f0edfc0556b9c214f8c2029b6390/src/utils/config.go)
2. Backup copies `.spa` archives and extracts `xpui.spa`/`login.spa`; pristine backups and preprocessed output are separate. [backup.go](https://github.com/spicetify/cli/blob/16840237b5c4f0edfc0556b9c214f8c2029b6390/src/backup/backup.go)
3. Preprocessing modifies HTML/JS/CSS, exposes internal APIs and adds wrappers. This snapshot also extracts webpack modules from CEF V8 context snapshots into local JS; that is a read/extraction path, not evidence of modifying the native snapshot itself. [preprocess.go](https://github.com/spicetify/cli/blob/16840237b5c4f0edfc0556b9c214f8c2029b6390/src/preprocess/preprocess.go)
4. Apply writes resources into Spotify Apps, copies helpers/extensions and inserts extension script tags into xpui HTML. The ordinary UI startup executes those scripts; there is no per-launch CLI or native dylib injection requirement. [apply command](https://github.com/spicetify/cli/blob/16840237b5c4f0edfc0556b9c214f8c2029b6390/src/cmd/apply.go), [apply implementation](https://github.com/spicetify/cli/blob/16840237b5c4f0edfc0556b9c214f8c2029b6390/src/apply/apply.go)
5. Backup/version state distinguishes stock, applied, mixed and invalid inputs; restore replaces patched resources with original archives. After Spotify updates, resources need re-application and sometimes a Spicetify update. This is resource restoration, not a full bundle/signing identity backup. [backup command](https://github.com/spicetify/cli/blob/16840237b5c4f0edfc0556b9c214f8c2029b6390/src/cmd/backup.go), [updating documentation](https://spicetify.app/docs/getting-started#updating)

### Signing and system-security boundary

Static inspection of this CLI snapshot found no macOS codesign/spctl/quarantine-clearing execution path. Do not interpret its SignPath policy as re-signing the modified macOS Spotify bundle.

Read-only local inspection: Spotify 1.2.94.583 arm64, Team `2FNC3A47ZF`, HR enabled, with allow-jit, allow-unsigned-executable-memory, disable-executable-page-protection and disable-library-validation; no allow-dyld-env. These are version-local facts. Existing CEF executes resource JS; that differs from loading foreign native Mach-O code. Library-validation exceptions do not exempt resource seals. [Apple library-validation entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.cs.disable-library-validation)

Pristine Spotify passed strict codesign verification; its CodeResources seals xpui.spa/login.spa. Replacing sealed resources invalidates the original outer seal even if untouched executable pages retain their signature/runtime flags. Separate executable-page validity, full bundle validity, Gatekeeper assessment and actual launch. Spicetify was not applied on this machine, so patched Spotify launch/distribution was not measured. [Apple TN2206](https://developer.apple.com/library/archive/technotes/tn2206/_index.html)

## Architecture comparison

| Surface | Spotify / Spicetify | Parsec / Orbit | Reuse |
| --- | --- | --- | --- |
| UI | CEF and local xpui SPA | libmatoya native UI, rollout-gated WKWebView code | No established equivalent local HTML entry |
| App resources | Apps/*.spa | icon, skel appdata and client dylib | Resource-only JS injection cannot host Orbit ABI |
| Extension | page JS/CSS/custom apps | loader, native ABI and optional Boa JS runtime | Reuse lifecycle, not payload format |
| Native policy | local Spotify has LV exception | pristine Parsec has no needed loading exceptions | Parsec needs changed application signing policy |
| Bootstrap | resource patch avoids executable mutation | small stub verifies hash and loads wx_main | Narrow native dependency patch is possible |
| Updates | SPA/modules change; re-apply | client dylib update and whole app/stub replacement | Track both layers independently |
| User state | ordinary Spotify state | ADR 0007 independent Orbit state | Normal launch does not imply same login |

Parsec 150.101.1 has id `tv.parsec.www`, LSBackgroundOnly, HR and Team `Y9MY52XZDB`; no allow-dyld-env or disable-LV. Local resources did not expose a supported arbitrary HTML/JS/plugin entry. UI-channel strings are not evidence of such an entry.

## Other projects

- **Vencord:** injects through Discord's Electron/ASAR startup and the JS module chain. Useful model for a minimal early entry and update-aware install; Parsec is not Electron and has no demonstrated ASAR entry. Pinned checkout: `595991ca27520868af4f9c48adf0c53ee971f1f2`. [Vencord installer](https://github.com/Vencord/Installer/blob/595991ca27520868af4f9c48adf0c53ee971f1f2/patcher.go)
- **SpotX macOS:** applies Spotify resource/native patches and re-signs in its installation workflow. Demonstrates the maintenance and signature tradeoff; blanket deep re-signing or quarantine deletion is not an Orbit recommendation. Pinned checkout: `d7c7b60f318f03884c02b4254e7bfd1701f17b70`. [SpotX source](https://github.com/SpotX-Official/SpotX-Bash/blob/d7c7b60f318f03884c02b4254e7bfd1701f17b70/spotx.sh)
- **Application wrapping / stub-copy host:** measured historically, avoids modifying the installed app but requires a different app entry. Keep as fallback/development mode, not the primary normal-official-app UX.
- **Watcher/redirect, DYLD environment, native attach:** previous research already mapped stripping, team validation, launch races and restricted entitlements. No new non-invasive in-process entry was established.

The project comparisons are static source analysis, not local deployments. Upstream behavior may have changed after the pinned snapshots.

## Candidate comparison

| Route | Feasibility | Normal installed-app launch | Maintenance / risk |
| --- | --- | --- | --- |
| Resource-only patch | no established Parsec entry | only if an entry exists | UI resources/API unknown; vendor resource seal affected |
| Static loader dependency + re-sign | leading candidate, later copy PoC passed | target; original path/id still unverified | signature identity/LV cost, header layout/version checks, full backup/restore |
| LSEnvironment + re-sign | requires different signing exceptions, not needed by later PoC | possible in theory, not selected | environment delivery and inheritance remain extra surfaces |
| Wrapper as main executable | possible hypothesis | can keep path but changes launch/bootstrap contract | argv/resources/self-exec handling, signing and permissions |
| Separate host app | historical smoke passed | user opens another app | lower vendor-file impact, misses central UX requirement |
| External companion | no native ABI equivalent | official app unchanged | only external functionality; excluded from current scope |

The selected experiment limits mutation to the bootstrap dependency and owned payloads. Full pristine bundle backup, explicit version state, transactional apply/restore and update detection are required. No route may depend on disabling SIP or global protections. See the [PoC results](native-dependency-poc.md) for measured behavior rather than these original feasibility predictions.
