use std::{env, fmt::Write, fs, path::PathBuf};

const CATALOG: &str = "i18n/en.tsv";

fn main() {
    println!("cargo::rerun-if-changed={CATALOG}");
    let catalog = fs::read_to_string(CATALOG).expect("read the English translation catalog");
    let mut entries = Vec::new();
    for (index, line) in catalog.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line_number = index + 1;
        let (source, english) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("{CATALOG}:{line_number}: expected a tab separator"));
        assert!(
            !source.is_empty() && !english.is_empty() && !english.contains('\t'),
            "{CATALOG}:{line_number}: expected two nonempty columns",
        );
        entries.push((source, english));
    }

    entries.sort_unstable_by_key(|&(source, _)| source);
    for pair in entries.windows(2) {
        assert_ne!(pair[0].0, pair[1].0, "{CATALOG}: duplicate source");
    }

    let mut output = String::from("static ENGLISH: &[(&str, &str)] = &[\n");
    for (source, english) in entries {
        writeln!(output, "    ({source:?}, {english:?}),").unwrap();
    }
    output.push_str("];\n");

    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    fs::write(output_dir.join("english.rs"), output).expect("write the English translation table");
}
