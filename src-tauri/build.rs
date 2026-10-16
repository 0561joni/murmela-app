fn main() {
    // MACOSX_DEPLOYMENT_TARGET comes from .cargo/config.toml.
    build_qwen_asr();
    set_release_date();

    tauri_build::build()
}

/// The day this build is released, as `MURMELA_RELEASE_DATE` (YYYY-MM-DD). A Pro key
/// unlocks builds released before its update year ends (see license.rs). The release
/// script sets the date; other builds use today's date in UTC.
fn set_release_date() {
    println!("cargo:rerun-if-env-changed=MURMELA_RELEASE_DATE");
    let date = match std::env::var("MURMELA_RELEASE_DATE") {
        Ok(date) => {
            let valid = date.len() == 10
                && date.char_indices().all(|(index, c)| {
                    if index == 4 || index == 7 { c == '-' } else { c.is_ascii_digit() }
                });
            assert!(valid, "MURMELA_RELEASE_DATE must look like 2026-10-16, not {date:?}");
            date
        }
        Err(_) => today_utc(),
    };
    println!("cargo:rustc-env=MURMELA_RELEASE_DATE={date}");
}

fn today_utc() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is before 1970")
        .as_secs() as i64;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn build_qwen_asr() {
    let vendor = "vendor/qwen-asr";
    for header in [
        "qwen_asr.h",
        "qwen_asr_audio.h",
        "qwen_asr_kernels.h",
        "qwen_asr_kernels_impl.h",
        "qwen_asr_safetensors.h",
        "qwen_asr_tokenizer.h",
    ] {
        println!("cargo:rerun-if-changed={vendor}/{header}");
    }
    let sources = [
        "qwen_asr.c",
        "qwen_asr_kernels.c",
        "qwen_asr_kernels_generic.c",
        "qwen_asr_kernels_neon.c",
        "qwen_asr_kernels_avx.c",
        "qwen_asr_audio.c",
        "qwen_asr_encoder.c",
        "qwen_asr_decoder.c",
        "qwen_asr_tokenizer.c",
        "qwen_asr_safetensors.c",
    ];

    let mut build = cc::Build::new();
    build
        .include(vendor)
        .define("USE_BLAS", None)
        .opt_level(3)
        .flag_if_supported("-ffast-math")
        .flag_if_supported("-Wno-unused-parameter");
    for source in sources {
        let path = format!("{vendor}/{source}");
        println!("cargo:rerun-if-changed={path}");
        build.file(path);
    }

    build.compile("qwen_asr");
    println!("cargo:rustc-link-lib=framework=Accelerate");
}
