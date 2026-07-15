# Win32/Vulkan validation runbook

The Win32 probe is compiled from macOS, but its milestone remains incomplete until these checks pass
on a physical Windows 10 or 11 x86-64 system.

## Setup

Install a current vendor driver exposing Vulkan 1.4, Rust 1.97, and a Vulkan SDK containing
`VK_LAYER_KHRONOS_validation` and `vulkaninfo`. No SDK library is needed to build Zinc because the
probe loads `vulkan-1.dll` at runtime.

Record the machine and driver before running:

```powershell
vulkaninfo --summary
rustc --version --verbose
```

## Automated finite run

From the repository root:

```powershell
$env:VK_LOADER_DEBUG = "error,warn"
cargo test -p zinc-vulkan-win32-triangle
cargo run -p zinc-vulkan-win32-triangle -- --frames 600
```

Success means exit code zero, a colored triangle was visible, and neither the Zinc validation
callback nor the loader printed a warning or error. Preserve the full output with the capability
report for the machine.

## Interactive lifecycle pass

Run without `--frames`, then:

1. Resize continuously, including very small sizes.
2. Minimize for several seconds and restore.
3. Maximize and restore.
4. If multiple displays are available, move the window between them.
5. Close with both the title-bar button and Alt+F4 on separate runs.

The triangle must remain stable, resize without stale frames, resume after minimize, remain
VSync-limited, and shut down without validation output.

## Evidence to return

- GPU model and driver version from `vulkaninfo --summary`.
- Whether the adapter is the GTX 1060-class baseline or another test tier.
- Console output from the finite run.
- Any validation message verbatim, plus the action that triggered it.
