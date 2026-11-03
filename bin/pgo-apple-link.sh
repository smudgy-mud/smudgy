#!/bin/sh
# Link an instrumented (`-Cprofile-generate`) Apple executable without `-dead_strip`.
#
# ld64 removes dead functions together with their `__llvm_prf_data` records
# (https://reviews.llvm.org/D25456). A dead-stripped instrumented binary therefore
# flushes a profile with no record for any function the linker dropped, and the
# profile-use compile, which sees every function of every crate again, reports each
# of them as having no profile data: tens of thousands of lines for the real
# application. Keeping the dead code in the training binary keeps every record, as
# the ELF and COFF linkers already do, so the strict diagnostics gate in bin/pgo.py
# holds on Apple targets too. Only the training binary links through this script;
# the profile-use build keeps rustc's default link line and is dead-stripped as
# usual. rustc drives the Apple link through `cc` and passes `-dead_strip` as one
# `-Wl,-dead_strip` argument; anything else is an unexpected link line.
set -eu
found=0
for argument; do
    shift
    case "$argument" in
        -Wl,-dead_strip) found=1; continue ;;
        @*) echo "pgo-apple-link: response file $argument is unsupported; shorten the link line" >&2; exit 1 ;;
    esac
    set -- "$@" "$argument"
done
if [ "$found" != 1 ]; then
    echo "pgo-apple-link: rustc passed no -Wl,-dead_strip to remove; inspect the link line" >&2
    exit 1
fi
exec cc "$@"
