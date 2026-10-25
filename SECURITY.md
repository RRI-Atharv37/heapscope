# Security policy

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private reporting: **Security > Report a
vulnerability** on https://github.com/RRI-Atharv37/heapscope. Include the version, what you did, and
what happened. You will get an answer within a few days; this is a one-person project, so please be
patient.

## Scope

HeapScope injects a DLL into processes you choose and reads `.hstrace` files. Interesting reports
include: a crafted trace file that crashes or exploits the reader, the control block or injection
accepting input from a process it should not, and the CI gate's licence check being bypassed
through a flaw (rather than by editing the binary).

Out of scope: needing administrator rights to attach to an elevated process, antivirus detections of
the injection technique (see [troubleshooting](docs/troubleshooting.md)), and anything that needs the
attacker to already control the profiled program.

## Supported versions

Only the latest release.
