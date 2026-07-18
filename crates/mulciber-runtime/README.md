# mulciber-runtime

Experimental game-loop timing and input snapshots for Mulciber.

The first slice provides a fixed-rate simulation accumulator, render interpolation, bounded catch-up,
scoped input transitions, and rendering suspend/resume coordination over `mulciber-platform`. It does
not yet own the native event pump, presentation pacing, process/OS suspension, display transitions,
jobs, or device recovery.
