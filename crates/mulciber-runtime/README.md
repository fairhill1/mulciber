# mulciber-runtime

Experimental game-loop timing and input snapshots for Mulciber.

The first slice provides a fixed-rate simulation accumulator, render interpolation, bounded catch-up,
scoped input transitions, and rendering suspend/resume coordination over `mulciber-platform`. Since
0.2.0, presented-cadence pacing diagnostics consume drained presentation feedback into cadence
estimates, interval distributions, and missed-interval counts. New in 0.3.0, the display-interval
frame pacer schedules simulation deltas as whole display intervals of the observed cadence with an
observable wall-clock fallback, so steady presentation no longer animates build-start jitter. The
crate does not yet own the native event pump, absolute frame-start scheduling, process/OS
suspension, display transitions, jobs, or device recovery.

Development, design contracts, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
