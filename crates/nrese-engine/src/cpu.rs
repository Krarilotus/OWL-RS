//! The CPU features the binary was compiled for, against the ones the machine has.
//!
//! Builds generate code for the building machine's CPU by default (`NRESE_TARGET_CPU`,
//! scripts/lib/target-cpu.sh). Started on a CPU without one of those features, the
//! program would stop with an illegal instruction somewhere in a query. [`missing`] lets
//! a program check at start and refuse with a message instead.

/// A feature, whether the binary was compiled to use it, and whether this CPU has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    pub name: &'static str,
    pub compiled: bool,
    pub available: bool,
}

/// The features that matter for the engine's hot loops (wider vectors, bit manipulation,
/// population counts), on this architecture.
///
/// On x86-64 the CPU is asked directly (CPUID, and XGETBV for the vector registers the
/// operating system saves). `is_x86_feature_detected!` can't serve here: it answers true
/// for every feature the binary was compiled with, without asking. On aarch64 the standard
/// detection has the same shortcut, so there `available` is only meaningful for features
/// the binary wasn't compiled with.
pub fn features() -> Vec<Feature> {
    #[cfg(target_arch = "x86_64")]
    {
        let cpu = x86::Cpu::read();
        macro_rules! feature {
            ($($name:tt),*) => {
                vec![$(Feature {
                    name: $name,
                    compiled: cfg!(target_feature = $name),
                    available: cpu.has($name),
                }),*]
            };
        }
        feature!(
            "sse4.2",
            "popcnt",
            "avx",
            "avx2",
            "bmi1",
            "bmi2",
            "fma",
            "lzcnt",
            "movbe",
            "avx512f",
            "avx512bw",
            "avx512dq",
            "avx512vl",
            "avx512vbmi",
            "avx512vpopcntdq"
        )
    }
    #[cfg(target_arch = "aarch64")]
    {
        macro_rules! feature {
            ($($name:tt),*) => {
                vec![$(Feature {
                    name: $name,
                    compiled: cfg!(target_feature = $name),
                    available: std::arch::is_aarch64_feature_detected!($name),
                }),*]
            };
        }
        feature!("neon", "crc", "lse", "sve", "sve2", "dotprod")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        Vec::new()
    }
}

/// The features the binary was compiled to use that this CPU lacks: if any, the binary
/// must not run here.
pub fn missing() -> Vec<&'static str> {
    features()
        .into_iter()
        .filter(|f| f.compiled && !f.available)
        .map(|f| f.name)
        .collect()
}

/// Exits the process with a message if this CPU lacks a feature the binary was compiled to
/// use. Call it first in `main`, before anything else runs: code compiled for the missing
/// features would stop with an illegal instruction instead.
#[cold]
#[inline(never)]
pub fn exit_if_missing() {
    let missing = missing();
    if !missing.is_empty() {
        eprintln!(
            "error: this binary was built for a CPU with {}, which this one lacks; rebuild it \
             here (NRESE_TARGET_CPU=native, the default) or for a lower level \
             (NRESE_TARGET_CPU=portable or x86-64-v3)",
            missing.join(", ")
        );
        std::process::exit(1);
    }
}

/// One line for configuration summaries: what the code uses, and what the CPU has beyond.
pub fn summary() -> String {
    let all = features();
    let compiled: Vec<&str> = all.iter().filter(|f| f.compiled).map(|f| f.name).collect();
    let unused: Vec<&str> = all
        .iter()
        .filter(|f| f.available && !f.compiled)
        .map(|f| f.name)
        .collect();
    let mut out = format!(
        "{} compiled for: {}",
        std::env::consts::ARCH,
        if compiled.is_empty() {
            "the architecture's baseline".to_owned()
        } else {
            compiled.join(" ")
        }
    );
    if !unused.is_empty() {
        out.push_str(&format!(
            "; this CPU also has {} (build with NRESE_TARGET_CPU=native to use them)",
            unused.join(" ")
        ));
    }
    out
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::{__cpuid, __cpuid_count, CpuidResult};

    /// The CPUID words the features are read from, and the register state the operating
    /// system enables (XCR0).
    pub(super) struct Cpu {
        leaf1: CpuidResult,
        leaf7: CpuidResult,
        extended: CpuidResult,
        xcr0: u64,
    }

    impl Cpu {
        pub(super) fn read() -> Self {
            // CPUID is a safe function on the supported toolchains: every x86-64 CPU has it; leaves
            // 0, 1 and 0x8000_0000 always exist; the others are read below their maximum.
            let (max, leaf1, max_extended) = (__cpuid(0).eax, __cpuid(1), __cpuid(0x8000_0000).eax);
            let leaf7 = if max >= 7 {
                __cpuid_count(7, 0)
            } else {
                CpuidResult {
                    eax: 0,
                    ebx: 0,
                    ecx: 0,
                    edx: 0,
                }
            };
            let extended = if max_extended >= 0x8000_0001 {
                __cpuid(0x8000_0001)
            } else {
                CpuidResult {
                    eax: 0,
                    ebx: 0,
                    ecx: 0,
                    edx: 0,
                }
            };
            // XGETBV only where the OS enabled it (OSXSAVE, leaf 1 ECX bit 27).
            let xcr0 = if leaf1.ecx & (1 << 27) != 0 {
                // SAFETY: OSXSAVE says XGETBV is available.
                unsafe { xgetbv0() }
            } else {
                0
            };
            Self {
                leaf1,
                leaf7,
                extended,
                xcr0,
            }
        }

        pub(super) fn has(&self, name: &str) -> bool {
            let bit = |word: u32, bit: u32| word & (1 << bit) != 0;
            // The OS saves the SSE and AVX registers (XCR0 bits 1, 2), and the AVX-512
            // ones (bits 5 to 7).
            let avx_state = self.xcr0 & 0b110 == 0b110;
            let avx512_state = avx_state && self.xcr0 & 0b1110_0000 == 0b1110_0000;
            let (c1, b7, c7, ce) = (
                self.leaf1.ecx,
                self.leaf7.ebx,
                self.leaf7.ecx,
                self.extended.ecx,
            );
            match name {
                "sse4.2" => bit(c1, 20),
                "popcnt" => bit(c1, 23),
                "movbe" => bit(c1, 22),
                "avx" => bit(c1, 28) && avx_state,
                "fma" => bit(c1, 12) && avx_state,
                "avx2" => bit(b7, 5) && avx_state,
                "bmi1" => bit(b7, 3),
                "bmi2" => bit(b7, 8),
                "lzcnt" => bit(ce, 5),
                "avx512f" => bit(b7, 16) && avx512_state,
                "avx512dq" => bit(b7, 17) && avx512_state,
                "avx512bw" => bit(b7, 30) && avx512_state,
                "avx512vl" => bit(b7, 31) && avx512_state,
                "avx512vbmi" => bit(c7, 1) && avx512_state,
                "avx512vpopcntdq" => bit(c7, 14) && avx512_state,
                _ => false,
            }
        }
    }

    /// XCR0, the register state the operating system saves on a context switch.
    #[target_feature(enable = "xsave")]
    unsafe fn xgetbv0() -> u64 {
        // SAFETY: the caller checked OSXSAVE.
        unsafe { std::arch::x86_64::_xgetbv(0) }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_test_binary_runs_where_it_was_built() {
        assert!(super::missing().is_empty(), "{:?}", super::missing());
        assert!(!super::summary().is_empty());
    }

    /// For features the binary wasn't compiled with, the standard detection asks the CPU
    /// too: both must agree.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn cpuid_agrees_with_the_standard_detection() {
        macro_rules! standard {
            ($name:expr, $($known:tt),*) => {
                match $name {
                    $($known => std::arch::is_x86_feature_detected!($known),)*
                    _ => unreachable!("an unlisted feature"),
                }
            };
        }
        for feature in super::features() {
            if !feature.compiled {
                let expected = standard!(
                    feature.name,
                    "sse4.2",
                    "popcnt",
                    "avx",
                    "avx2",
                    "bmi1",
                    "bmi2",
                    "fma",
                    "lzcnt",
                    "movbe",
                    "avx512f",
                    "avx512bw",
                    "avx512dq",
                    "avx512vl",
                    "avx512vbmi",
                    "avx512vpopcntdq"
                );
                assert_eq!(feature.available, expected, "{}", feature.name);
            }
        }
    }
}
