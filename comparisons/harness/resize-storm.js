// KWin scripting resize storm for the Gate 2 lifecycle comparison.
//
// %NEEDLE% is substituted by run-resize-storm.sh with a window-caption substring so the same
// storm drives each implementation. The storm applies 350 server-side frame-geometry steps at
// 10 ms intervals (the same walk recorded in docs/linux-validation.md), then closes the window.
// If geometry assignment has no effect on the target compositor, it falls back to maximize
// toggling so a run never silently does nothing.

function findTarget() {
    var list = (typeof workspace.windowList === 'function')
        ? workspace.windowList()
        : workspace.clientList();
    for (var i = 0; i < list.length; i++) {
        if (list[i].caption && list[i].caption.indexOf('%NEEDLE%') !== -1) {
            return list[i];
        }
    }
    return null;
}

var target = findTarget();
if (target) {
    var step = 0;
    var toggles = 0;
    var initialWidth = target.frameGeometry.width;
    var assignmentWorked = false;
    var timer = new QTimer();
    timer.interval = 10;
    timer.timeout.connect(function () {
        step += 1;
        if (step <= 350) {
            target.frameGeometry = {
                x: target.frameGeometry.x,
                y: target.frameGeometry.y,
                width: 420 + ((step * 37) % 1200),
                height: 320 + ((step * 23) % 700)
            };
            if (target.frameGeometry.width !== initialWidth) {
                assignmentWorked = true;
            }
            return;
        }
        if (assignmentWorked) {
            timer.stop();
            print('comparison storm finished via geometry assignment');
            target.closeWindow();
            return;
        }
        timer.interval = 300;
        toggles += 1;
        if (toggles > 30) {
            timer.stop();
            print('comparison storm finished via maximize toggling');
            target.closeWindow();
            return;
        }
        var on = (toggles % 2) === 1;
        target.setMaximize(on, on);
    });
    timer.start();
    print('comparison resize storm started at width ' + initialWidth);
} else {
    print('comparison storm target window not found: %NEEDLE%');
}
