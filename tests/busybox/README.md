# BusyBox testsuite for epkg applets

Run the external BusyBox testsuite (at BUSYBOX_TESTSUITE or /c/busybox/testsuite)
against epkg's busybox applets with no or minimal changes to the BusyBox tree.

## How it works

- The BusyBox runtest expects a directory (bindir) containing a "busybox" binary
  that (1) when run with no args prints "Currently defined functions: applet1, ..."
  and (2) when run as argv[0]=applet or "busybox applet" runs that applet.
- We create a temporary bindir with a wrapper script that does both: it runs
  "epkg busybox <applet> ..." and, when run with no args, lists epkg applets in
  the same format so runtest discovers only applets we implement.
- Tests that call "busybox cat" or "cat" then run epkg's implementation without
  modifying any BusyBox testsuite file.

## Scripts

- **busybox-runtest.sh** – Run the BusyBox runtest in place; only applets implemented
  by epkg are tested; others are skipped. Optionally restrict to given applets.
- **run-one.sh** – Run tests for a single applet (e.g. ./run-one.sh cat).

## Environment / options

- **BUSYBOX_TESTSUITE** – Path to BusyBox testsuite directory (default: busybox sub-directory
  in epkg repo, e.g. /c/epkg/git/busybox/testsuite when epkg is in /c/epkg).
- **EPKG_BIN** – Path to epkg binary (default: from tests/common.sh).
- **EPKG_BUSYBOX_SKIP_FEATURES** – Comma-separated list of BusyBox CONFIG_ feature
  names to treat as "not set" so optional(FEATURE_*) tests are skipped (e.g. if
  epkg does not implement that feature yet). Example: EPKG_BUSYBOX_SKIP_FEATURES=FEATURE_CATV,FEATURE_CATN
- **VERBOSE=1** – Passed through to runtest for verbose output.

## Skipping tests

- If epkg has no such applet: runtest skips that applet (no LINKSDIR entry).
- If a test requires a feature epkg does not implement: either set
  EPKG_BUSYBOX_SKIP_FEATURES so the generated .config marks that feature as not
  set (optional() will skip), or implement the feature in epkg so the test passes.

## Requirements

- BusyBox testsuite directory must be **writable**: the harness compiles a small
  helper (echo-ne) there when the system `echo` does not support `-n`/`-e`.

## Example

  cd /c/epkg/tests/busybox
  sh busybox-runtest.sh
  sh busybox-runtest.sh cat ls
  EPKG_BUSYBOX_SKIP_FEATURES=FEATURE_CATV sh busybox-runtest.sh cat
