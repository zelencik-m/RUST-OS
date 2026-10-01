use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const PROGRAMS: &[(&str, &str)] = &[
    ("elf_loader_test", "tests/user_programs"),
    ("ring3_exception_test", "tests/user_programs"),
    ("syscall_test", "tests/user_programs"),
    ("vfs_test", "tests/user_programs"),
    ("fs_tree_test", "tests/user_programs"),
    ("thread_b", "user_programs"),
    ("thread_c", "user_programs"),
];

fn run(command: &mut Command, description: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("failed to {description}: {error}"));
    assert!(status.success(), "failed to {description}: {status}");
}

fn rust_lld() -> PathBuf {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("failed to query the Rust sysroot");
    assert!(output.status.success(), "rustc --print sysroot failed");

    let sysroot = String::from_utf8(output.stdout).expect("Rust sysroot is not UTF-8");
    let host = env::var("HOST").expect("Cargo did not provide HOST");
    Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin/rust-lld")
}

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing OUT_DIR"));
    let linker_script = Path::new("user_programs/linker.ld");
    let syscall_header = Path::new("user_programs/syscalls.h");
    let linker = rust_lld();

    println!("cargo:rerun-if-changed={}", linker_script.display());
    println!("cargo:rerun-if-changed={}", syscall_header.display());

    for &(program, source_dir) in PROGRAMS {
        let source_dir = Path::new(source_dir);
        let source = source_dir.join(format!("{program}.c"));
        let object = out_dir.join(format!("{program}.o"));
        let elf = out_dir.join(format!("{program}.elf"));

        println!("cargo:rerun-if-changed={}", source.display());

        run(
            Command::new("clang")
                .arg("--target=x86_64-unknown-none-elf")
                .args([
                    "-std=c11",
                    "-O2",
                    "-ffreestanding",
                    "-fno-builtin",
                    "-fno-pic",
                    "-fno-pie",
                    "-fno-stack-protector",
                    "-mcmodel=large",
                    "-mno-red-zone",
                    "-mno-mmx",
                    "-mno-sse",
                    "-mno-sse2",
                ])
                .arg("-I")
                .arg("user_programs")
                .arg("-c")
                .arg(&source)
                .arg("-o")
                .arg(&object),
            &format!("compile {}", source.display()),
        );

        run(
            Command::new(&linker)
                .args(["-flavor", "gnu", "-m", "elf_x86_64", "-nostdlib"])
                .arg("-T")
                .arg(linker_script)
                .arg("-o")
                .arg(&elf)
                .arg(&object),
            &format!("link {}", elf.display()),
        );
    }
}
