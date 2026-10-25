# Code signing policy

Free code signing for HeapScope's open-source releases is provided by
[SignPath.io](https://signpath.io), with a certificate from the
[SignPath Foundation](https://signpath.org). **This applies once the SignPath Foundation has approved the
project; until then, releases are published unsigned and say so in their notes.**

## What is signed

`heapscope.exe` and `heapscope_capture.dll` in the GitHub Releases of this repository, built by
the `release` workflow ([`.github/workflows/release.yml`](.github/workflows/release.yml)) from a
tagged commit. Nothing else is signed with this certificate. The paid `heapscope-ci` binary is
not part of this repository and is not covered.

## Roles

| Role | Who |
|---|---|
| Committer and reviewer | [Atharv Dewangan](https://github.com/RRI-Atharv37) |
| Approver of signing requests | [Atharv Dewangan](https://github.com/RRI-Atharv37) |

Contributions from others are merged only by the committer after review. Everyone with these roles
uses multi-factor authentication on GitHub and SignPath.

## Privacy

HeapScope does not send data anywhere. Profiling, analysis and reports run on your machine, and the
CI gate checks its licence locally. The only network access is Windows downloading symbols from the
server in `_NT_SYMBOL_PATH`, if you set one.

## Uninstall

HeapScope has no installer. Delete the folder you unpacked. A profiled program loses the
`heapscope_capture.dll` hooks when it exits; an attached program keeps the DLL loaded (dormant)
until it exits.

## Verifying a download

Each release lists the SHA-256 of the zip and ships it as `<zip>.sha256`. Signed releases also have
a valid Authenticode signature on both binaries (right-click > Properties > Digital Signatures).
Report a suspicious download as described in [SECURITY.md](SECURITY.md).
