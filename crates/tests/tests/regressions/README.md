# Regression reproducers

One file per fuzzer-found divergence, named `bug_NNN.rs`, containing the
minimal input sequence and a comment explaining what broke and why.
See `docs/BUGS.md` for the full log.

Empty so far: the differential campaign on the finished engine found
zero divergences (see BUGS.md §2 for the volume and the mutation tests
that prove the harness can detect the classic bugs).
