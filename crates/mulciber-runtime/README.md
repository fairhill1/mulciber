# mulciber-runtime

Experimental game-loop timing and input snapshots for Mulciber.

The first slice provides a fixed-rate simulation accumulator, render interpolation, bounded catch-up,
scoped input transitions, and rendering suspend/resume coordination over `mulciber-platform`. Since
0.2.0, presented-cadence pacing diagnostics consume drained presentation feedback into cadence
estimates, interval distributions, and missed-interval counts. Since 0.3.0, the display-interval
frame pacer schedules simulation deltas as whole display intervals of the observed cadence with an
observable wall-clock fallback, so steady presentation no longer animates build-start jitter. New
in 0.4.0, `Runtime` owns that pacer: drain presentation feedback into `Runtime::record_presented`
and `begin_frame` is paced with no further wiring, with the fallback observable per frame through
`RuntimeFrame::schedule` and in aggregate through `Runtime::pacing_report`. The crate does not yet
own the native event pump, absolute frame-start scheduling, process/OS suspension, display
transitions, jobs, or device recovery.

Development, design contracts, and recorded validation evidence live in the
[Mulciber repository](https://github.com/fairhill1/mulciber).
