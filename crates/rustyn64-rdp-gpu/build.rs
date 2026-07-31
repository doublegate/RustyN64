//! Compile the vendored parallel-rdp and the flat shim over it — but **only**
//! when the `gpu-rdp` feature is on (ADR 0014 §1).
//!
//! Everything here is conditional on that feature for a reason worth stating: a
//! `build.rs` that always ran would make a C++ compiler and the submodule
//! mandatory for every contributor, including the ones who never enable the
//! backend. The ADR's kill criteria include "the Vulkan dependency cannot be made
//! optional", so this file is where that promise is either kept or broken.

/// The `vulkan/` and `util/` sources upstream's `config.mk` names explicitly.
///
/// NOT a wildcard, and the difference matters: config.mk lists 22 of the 27
/// `vulkan/*.cpp` files, and the omissions are load-bearing —
/// `device_fossilize.cpp` includes a `thread_group.hpp` this standalone tree
/// does not ship, so globbing the directory does not merely add dead code, it
/// fails to compile.
const ENUMERATED: &[&str] = &[
    "vulkan/buffer.cpp",
    "vulkan/buffer_pool.cpp",
    "vulkan/command_buffer.cpp",
    "vulkan/command_pool.cpp",
    "vulkan/context.cpp",
    "vulkan/cookie.cpp",
    "vulkan/descriptor_set.cpp",
    "vulkan/device.cpp",
    "vulkan/event_manager.cpp",
    "vulkan/fence.cpp",
    "vulkan/fence_manager.cpp",
    "vulkan/image.cpp",
    "vulkan/indirect_layout.cpp",
    "vulkan/memory_allocator.cpp",
    "vulkan/pipeline_event.cpp",
    "vulkan/query_pool.cpp",
    "vulkan/render_pass.cpp",
    "vulkan/sampler.cpp",
    "vulkan/semaphore.cpp",
    "vulkan/semaphore_manager.cpp",
    "vulkan/shader.cpp",
    "vulkan/texture/texture_format.cpp",
    "util/arena_allocator.cpp",
    "util/logging.cpp",
    "util/thread_id.cpp",
    "util/aligned_alloc.cpp",
    "util/timer.cpp",
    "util/timeline_trace_file.cpp",
    "util/environment.cpp",
    "util/thread_name.cpp",
];

fn main() {
    println!("cargo:rerun-if-changed=shim/prdp_shim.h");
    println!("cargo:rerun-if-changed=shim/prdp_shim.cpp");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var_os("CARGO_FEATURE_GPU_RDP").is_none() {
        return;
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/parallel-rdp-standalone");

    // A submodule that was never initialized is an empty directory, and the
    // error `cc` would give for that ("no such file") sends people looking at
    // their compiler rather than at git. Say the actual thing.
    assert!(
        root.join("parallel-rdp/rdp_device.hpp").is_file(),
        "the parallel-rdp submodule is not checked out.\n\
         Run: git submodule update --init vendor/parallel-rdp-standalone\n\
         (looked in {})",
        root.display()
    );

    // Rebuild when the vendored tree changes — a submodule bump, or a local
    // edit while debugging. Emitted here rather than beside the shim's own
    // triggers above, because naming a path that does not exist makes cargo
    // rerun this script unconditionally, and the path only exists once the
    // submodule is checked out. Cargo walks a directory recursively, so the
    // four source roots cover every file actually compiled below.
    for dir in ["parallel-rdp", "vulkan", "util", "volk"] {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(root.join("parallel-rdp"))
        .include(root.join("volk"))
        .include(root.join("vulkan"))
        .include(root.join("vulkan-headers/include"))
        .include(root.join("util"))
        .include("shim")
        // Upstream's own warning posture: it is vendored third-party code and
        // this project's lint level is not its lint level. Warnings from OUR
        // shim still surface, because it is compiled in the same unit and
        // nothing suppresses them selectively.
        .warnings(false)
        .file("shim/prdp_shim.cpp");

    // `parallel-rdp/*.cpp` is a wildcard in upstream's `config.mk`, so it is a
    // wildcard here. `vulkan/` and `util/` are not — see [`ENUMERATED`].
    let mut wildcard: Vec<_> = std::fs::read_dir(root.join("parallel-rdp"))
        .expect("parallel-rdp/ is unreadable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "cpp"))
        .collect();
    // Sorted so the compile order — and therefore any diagnostic ordering —
    // does not depend on the filesystem's whims.
    wildcard.sort();
    for f in wildcard {
        build.file(f);
    }

    for rel in ENUMERATED {
        let p = root.join(rel);
        assert!(
            p.is_file(),
            "upstream config.mk names {rel}, which is missing from the submodule.\n\
             The vendored tree and this list have diverged — reconcile against \
             vendor/parallel-rdp-standalone/config.mk."
        );
        build.file(p);
    }

    // volk is C, not C++, so it gets its own unit.
    cc::Build::new()
        .file(root.join("volk/volk.c"))
        .include(root.join("volk"))
        .include(root.join("vulkan-headers/include"))
        .warnings(false)
        .compile("volk");

    build.compile("parallel_rdp_shim");

    // The C++ standard library is NOT linked by hand. `cc` already emits it for
    // a `.cpp(true)` build and picks the right one per target — `libstdc++` on
    // Linux, `libc++` on macOS. Naming `stdc++` here, as an earlier version did,
    // would have broken the macOS leg of the CI matrix while looking correct on
    // the machine it was written on.
    //
    // There is also deliberately no `-lvulkan`: volk `dlopen`s the loader at
    // runtime. That is what lets a build succeed on a machine with no Vulkan ICD
    // and fail at `prdp_create` instead, which is a far better diagnostic than a
    // link error — and it is what makes the CI job above meaningful on a runner
    // with no GPU.
    //
    // `-ldl` comes from upstream's `config.mk`, and only off Windows. The
    // condition is on the TARGET, not on the host: `#[cfg(unix)]` in a build
    // script describes the machine doing the compiling, which is the wrong
    // question when cross-compiling.
    if std::env::var("CARGO_CFG_TARGET_FAMILY").as_deref() == Ok("unix") {
        println!("cargo:rustc-link-lib=dylib=dl");
    }
}
